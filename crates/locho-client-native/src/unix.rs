//! Safe ownership and cleanup for filesystem Unix stream listeners.

#![cfg(unix)]

use std::{
    io,
    path::{Component, Path, PathBuf},
};
use tokio::net::UnixListener;
mod filesystem;
use filesystem::{Identity, Parent};

/// A Unix listener that removes only the socket created by this value.
pub struct UnixListenerGuard {
    listener: Option<UnixListener>,
    path: PathBuf,
    parent: Parent,
    identity: Identity,
}

impl UnixListenerGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn accept(&self) -> io::Result<(tokio::net::UnixStream, ())> {
        self.listener
            .as_ref()
            .expect("UnixListenerGuard used after drop")
            .accept()
            .await
            .map(|(stream, _)| (stream, ()))
    }
}

impl Drop for UnixListenerGuard {
    fn drop(&mut self) {
        // Close the descriptor before checking/removing the directory entry.
        self.listener.take();
        self.parent.unlink_owned(self.identity);
    }
}

pub fn bind_unix(path: &Path) -> io::Result<UnixListenerGuard> {
    validate_path(path)?;
    let parent = Parent::open(path)?;
    parent.require_absent()?;
    let listener = parent.bind().map_err(|error| {
        if error.kind() == io::ErrorKind::AddrInUse {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "Unix socket path {} is already in use or stale; verify and remove it explicitly",
                    path.display()
                ),
            )
        } else {
            error
        }
    })?;
    let identity = parent.identity()?;
    let guard = UnixListenerGuard {
        listener: None,
        path: path.to_owned(),
        parent,
        identity,
    };
    guard.parent.chmod_socket(identity)?;
    listener.set_nonblocking(true)?;
    let mut guard = guard;
    guard.listener = Some(UnixListener::from_std(listener)?);
    Ok(guard)
}

fn validate_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unix socket path must be an absolute, non-empty filesystem path",
        ));
    }
    if path.file_name().is_none()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unix socket path must name a file and contain no '..' components",
        ));
    }
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    if bytes.ends_with(b"/") || bytes.ends_with(b"/.") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unix socket path must not end with a directory separator or '/.'",
        ));
    }
    #[cfg(target_os = "macos")]
    const MAX_PATH_BYTES: usize = 103;
    #[cfg(not(target_os = "macos"))]
    const MAX_PATH_BYTES: usize = 107;
    if bytes.contains(&0) || bytes.len() > MAX_PATH_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unix socket path contains NUL or exceeds the platform socket-path limit",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    fn test_directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "locho-u-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    #[tokio::test]
    async fn binds_mode_0600_and_removes_owned_socket() {
        let directory = test_directory();
        let path = directory.join("s");
        let cwd = std::env::current_dir().unwrap();
        let listener = bind_unix(&path).unwrap();
        assert_eq!(
            std::env::current_dir().unwrap(),
            cwd,
            "binding must not change the process cwd"
        );
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        drop(listener);
        assert!(!path.exists());
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn refuses_existing_entry_without_mutating_it() {
        let directory = test_directory();
        let path = directory.join("existing");
        std::fs::write(&path, b"keep").unwrap();
        assert!(bind_unix(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn cleanup_preserves_replacement_file_directory_and_symlink() {
        let directory = test_directory();
        let path = directory.join("s");
        let moved = directory.join("old");
        let target = directory.join("target");
        std::fs::write(&target, b"untouched").unwrap();
        for replacement in ["file", "directory", "symlink"] {
            let listener = bind_unix(&path).unwrap();
            std::fs::rename(&path, &moved).unwrap();
            match replacement {
                "file" => std::fs::write(&path, b"replacement").unwrap(),
                "directory" => std::fs::create_dir(&path).unwrap(),
                "symlink" => std::os::unix::fs::symlink(&target, &path).unwrap(),
                _ => unreachable!(),
            }
            let identity = std::fs::symlink_metadata(&path).unwrap();
            drop(listener);
            let remaining = std::fs::symlink_metadata(&path).unwrap();
            assert_eq!(remaining.dev(), identity.dev());
            assert_eq!(remaining.ino(), identity.ino());
            assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
            if remaining.is_dir() {
                std::fs::remove_dir(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
            std::fs::remove_file(&moved).unwrap();
        }
        std::fs::remove_file(target).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn refuses_live_stale_and_dangling_symlink_entries() {
        let directory = test_directory();
        let path = directory.join("s");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert_eq!(
            bind_unix(&path).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        let client = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();
        drop((client, server, listener));
        assert_eq!(
            bind_unix(&path).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_socket());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(directory.join("missing"), &path).unwrap();
        assert_eq!(
            bind_unix(&path).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn rejects_insecure_missing_parents_and_invalid_paths() {
        let directory = test_directory();
        let path = directory.join("s");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(
            bind_unix(&path).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!path.exists());
        assert_eq!(std::fs::metadata(&directory).unwrap().mode() & 0o777, 0o750);
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(bind_unix(&directory.join("missing/s")).is_err());
        for path in [
            PathBuf::new(),
            "relative.sock".into(),
            directory.join("../s"),
            directory.join("s".repeat(108)),
            directory.join("s/"),
            directory.join("s/."),
        ] {
            assert_eq!(
                bind_unix(&path).err().unwrap().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn pinned_parent_survives_path_substitution_during_bind_and_cleanup() {
        let root = test_directory();
        let original = root.join("p");
        let moved = root.join("m");
        let other = root.join("o");
        for path in [&original, &other] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = original.join("s");
        let parent = Parent::open(&path).unwrap();
        parent.require_absent().unwrap();
        std::fs::rename(&original, &moved).unwrap();
        std::os::unix::fs::symlink(&other, &original).unwrap();
        let listener = parent.bind().unwrap();
        let identity = parent.identity().unwrap();
        parent.chmod_socket(identity).unwrap();
        assert!(moved.join("s").exists());
        assert!(!other.join("s").exists());
        std::fs::write(other.join("s"), b"keep").unwrap();
        drop(listener);
        parent.unlink_owned(identity);
        assert!(!moved.join("s").exists());
        assert_eq!(std::fs::read(other.join("s")).unwrap(), b"keep");
        std::fs::remove_file(original).unwrap();
        std::fs::remove_file(other.join("s")).unwrap();
        std::fs::remove_dir(other).unwrap();
        std::fs::remove_dir(moved).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn substitution_before_chmod_never_changes_symlink_target() {
        let root = test_directory();
        let path = root.join("s");
        let parent = Parent::open(&path).unwrap();
        let listener = parent.bind().unwrap();
        let identity = parent.identity().unwrap();
        std::fs::rename(&path, root.join("old")).unwrap();
        let target = root.join("target");
        std::fs::write(&target, b"keep").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(parent.chmod_socket(identity).is_err());
        parent.unlink_owned(identity);
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o640);
        drop(listener);
        for path in [&path, &target, &root.join("old")] {
            std::fs::remove_file(path).unwrap();
        }
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn refuses_entry_created_between_preflight_and_bind_and_unsafe_ancestry() {
        let root = test_directory();
        let path = root.join("s");
        let parent = Parent::open(&path).unwrap();
        parent.require_absent().unwrap();
        std::fs::write(&path, b"keep").unwrap();
        assert!(parent.bind().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        std::fs::remove_file(path).unwrap();
        let private = root.join("p");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(Parent::open(&private.join("s")).is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir(private).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rejects_acl_grants_even_when_parent_mode_is_private() {
        let root = test_directory();
        let status = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow search"])
            .arg(&root)
            .status()
            .unwrap();
        assert!(status.success());
        let rejected = Parent::open(&root.join("s")).is_err();
        let status = std::process::Command::new("/bin/chmod")
            .args(["-a", "everyone allow search"])
            .arg(&root)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir(root).unwrap();
        assert!(
            rejected,
            "extended ACL allow entries must not bypass private-directory checks"
        );
    }
}
