//! Safe ownership and cleanup for filesystem Unix stream listeners.

#![cfg(unix)]

use std::{
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};
use tokio::net::UnixListener;

const SOCKET_MODE: u32 = 0o600;

/// A Unix listener that removes only the socket created by this value.
pub struct UnixListenerGuard {
    listener: Option<UnixListener>,
    path: PathBuf,
    device: u64,
    inode: u64,
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
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.dev() == self.device && metadata.ino() == self.inode {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn bind_unix(path: &Path) -> io::Result<UnixListenerGuard> {
    validate_path(path)?;
    validate_private_parent(path)?;
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "Unix socket path {} already exists; verify and remove a stale socket explicitly",
                path.display()
            ),
        ));
    }

    let listener = UnixListener::bind(path).map_err(|error| {
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
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => metadata,
        Ok(_) => {
            drop(listener);
            return Err(io::Error::other(
                "Unix listener path was replaced during bind",
            ));
        }
        Err(error) => {
            drop(listener);
            return Err(error);
        }
    };
    let guard = UnixListenerGuard {
        listener: Some(listener),
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE))
    {
        drop(guard);
        return Err(error);
    }
    let final_metadata = std::fs::symlink_metadata(path)?;
    if final_metadata.dev() != guard.device
        || final_metadata.ino() != guard.inode
        || !final_metadata.file_type().is_socket()
    {
        drop(guard);
        return Err(io::Error::other(
            "Unix listener path changed during permission setup",
        ));
    }
    // Keep the guard alive while the caller owns the listener.
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

fn validate_private_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unix socket has no parent directory",
        )
    })?;
    // geteuid has no pointer or process-global mutation requirements.
    let effective_uid = unsafe { libc::geteuid() };
    let mut current = PathBuf::new();
    for component in parent.components() {
        match component {
            Component::RootDir => current.push("/"),
            Component::Normal(value) => current.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Unix socket parent contains an invalid path component",
                ))
            }
        }
        let component_metadata = std::fs::symlink_metadata(&current)?;
        if component_metadata.file_type().is_symlink()
            && component_metadata.uid() != effective_uid
            && component_metadata.uid() != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "Unix socket parent {} contains a foreign symlink",
                    current.display()
                ),
            ));
        }
    }
    // Resolve existing parent components so platform-managed aliases such as
    // macOS /tmp -> /private/tmp work. The final socket entry is still checked
    // with symlink_metadata and is never followed when deciding ownership or
    // whether an existing path may be used.
    let resolved_parent = std::fs::canonicalize(parent)?;
    let metadata = std::fs::symlink_metadata(&resolved_parent)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("Unix socket parent {} is not a directory", parent.display()),
        ));
    }
    // The parent is deliberately checked before bind. A 0700 directory owned by
    // the effective user prevents another user from racing the bind/chmod window.
    if metadata.uid() != effective_uid || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Unix socket parent {} must be owned by the effective user and mode 0700",
                resolved_parent.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let listener = bind_unix(&path).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(metadata.mode() & 0o777, SOCKET_MODE);
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
        ] {
            assert_eq!(
                bind_unix(&path).err().unwrap().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        std::fs::remove_dir(directory).unwrap();
    }
}
