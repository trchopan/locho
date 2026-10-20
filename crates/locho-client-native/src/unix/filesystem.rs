//! Directory-anchored filesystem operations. Root and same-UID processes are
//! trusted; do not race listener paths from another process running as this UID.
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io,
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
    device: u64,
    inode: u64,
}

pub(super) struct Parent {
    directory: File,
    name: CString,
}

fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn trusted_directory(metadata: &std::fs::Metadata, uid: u32) -> io::Result<()> {
    if !metadata.is_dir() || (metadata.uid() != 0 && metadata.uid() != uid) {
        return Err(denied(
            "Unix listener ancestry must be directories owned by root or the effective user",
        ));
    }
    // Sticky directories protect each root/user-owned child (e.g. /tmp).
    if metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0 {
        return Err(denied(
            "Unix listener ancestry must not be group/other writable without the sticky bit",
        ));
    }
    Ok(())
}

impl Parent {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        // SAFETY: geteuid has no pointer or mutation requirements.
        let uid = unsafe { libc::geteuid() };
        let parent = path
            .parent()
            .ok_or_else(|| denied("Unix listener has no parent"))?;
        let mut prefix = PathBuf::new();
        for component in parent.components() {
            prefix.push(component.as_os_str());
            let entry = std::fs::symlink_metadata(&prefix)?;
            // Permit root-owned system aliases (/var, /tmp on macOS), not
            // mutable user/foreign symlinks. The canonical walk below is no-follow.
            if entry.file_type().is_symlink() && entry.uid() != 0 {
                return Err(denied("Unix listener ancestry must not contain non-root symlinks; use the real directory path"));
            }
            trusted_directory(&std::fs::metadata(&prefix)?, uid)?;
        }
        let resolved = std::fs::canonicalize(parent)?;
        let mut directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")?;
        trusted_directory(&directory.metadata()?, uid)?;
        reject_allow_acl(&directory)?;
        for component in resolved.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            let name = CString::new(name.as_bytes())?;
            // SAFETY: directory is live and name is a NUL-terminated component.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: openat returned a new descriptor owned by this File.
            directory = unsafe { File::from_raw_fd(fd) };
            trusted_directory(&directory.metadata()?, uid)?;
            reject_allow_acl(&directory)?;
        }
        let metadata = directory.metadata()?;
        if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(denied("Unix socket parent must be owned by the effective user with no group/other access (mode 0700)"));
        }
        Ok(Self {
            directory,
            name: CString::new(path.file_name().expect("validated socket name").as_bytes())?,
        })
    }

    fn stat(&self) -> io::Result<libc::stat> {
        let mut metadata = MaybeUninit::uninit();
        // SAFETY: pointers are valid; only assume initialization on success.
        let result = unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                self.name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatat initialized metadata.
        Ok(unsafe { metadata.assume_init() })
    }

    pub(super) fn require_absent(&self) -> io::Result<()> {
        match self.stat() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Unix socket path already exists; verify and remove a stale socket explicitly",
            )),
        }
    }

    // libc's dev_t/ino_t widths and signedness vary between Unix targets.
    #[allow(clippy::unnecessary_cast)]
    pub(super) fn identity(&self) -> io::Result<Identity> {
        let metadata = self.stat()?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFSOCK {
            return Err(denied("Unix listener entry is not the expected socket"));
        }
        Ok(Identity {
            device: metadata.st_dev as u64,
            inode: metadata.st_ino as u64,
        })
    }

    pub(super) fn chmod_socket(&self, identity: Identity) -> io::Result<()> {
        if self.identity()? != identity {
            return Err(denied("Unix socket was replaced before permission setup"));
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::FileTypeExt;
            // Pin the actual filesystem socket inode, not the socket descriptor
            // (fchmod on a Linux socket descriptor changes a different inode).
            // SAFETY: live directory descriptor and valid component string.
            let fd = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    self.name.as_ptr(),
                    libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fd is a newly owned descriptor.
            let entry = unsafe { File::from_raw_fd(fd) };
            let metadata = entry.metadata()?;
            if !metadata.file_type().is_socket()
                || metadata.dev() != identity.device
                || metadata.ino() != identity.inode
            {
                return Err(denied("Unix socket changed during permission setup"));
            }
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                format!("/proc/self/fd/{}", entry.as_raw_fd()),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }
        #[cfg(target_os = "macos")]
        {
            // SAFETY: valid directory/name, and NOFOLLOW prevents changing a
            // symlink target. Concurrent same-UID entry replacement is untrusted
            // application behavior, not a separate OS security boundary.
            if unsafe {
                libc::fchmodat(
                    self.directory.as_raw_fd(),
                    self.name.as_ptr(),
                    0o600,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        if self.identity()? != identity {
            return Err(denied("Unix socket changed during permission setup"));
        }
        Ok(())
    }

    pub(super) fn unlink_owned(&self, identity: Identity) {
        if self.identity().ok() == Some(identity) {
            // SAFETY: fd/name are live. There is no portable compare-and-unlink;
            // the pinned private directory excludes other UIDs, not this UID.
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0);
            }
        }
    }

    pub(super) fn bind(&self) -> io::Result<std::os::unix::net::UnixListener> {
        #[cfg(target_os = "linux")]
        {
            std::os::unix::net::UnixListener::bind(
                Path::new(&format!("/proc/self/fd/{}", self.directory.as_raw_fd()))
                    .join(std::ffi::OsStr::from_bytes(self.name.as_bytes())),
            )
        }
        #[cfg(target_os = "macos")]
        {
            let directory = self.directory.try_clone()?;
            let name = self.name.clone();
            // Darwin has no bindat. A dedicated thread's cwd is private to that
            // thread and dies with it; never change the process cwd or a pooled
            // Tokio worker's cwd. No async code executes in this scope.
            std::thread::Builder::new()
                .name("locho-unix-bind".into())
                .spawn(move || {
                    unsafe extern "C" {
                        fn pthread_fchdir_np(fd: libc::c_int) -> libc::c_int;
                    }
                    // SAFETY: the directory fd is valid; this thread is run-owned.
                    if unsafe { pthread_fchdir_np(directory.as_raw_fd()) } != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    std::os::unix::net::UnixListener::bind(Path::new(std::ffi::OsStr::from_bytes(
                        name.as_bytes(),
                    )))
                })?
                .join()
                .map_err(|_| io::Error::other("Unix bind thread failed"))?
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unix listeners require Linux or macOS",
            ))
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn reject_allow_acl(_directory: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn reject_allow_acl(directory: &File) -> io::Result<()> {
    // Darwin ACL grants can override mode bits. Conservatively reject allow
    // entries instead of attempting to resolve UUID/group membership. Standard
    // deny-only system ACLs (e.g. /Users) do not weaken the ancestry checks.
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(
            acl: *mut libc::c_void,
            which: libc::c_int,
            entry: *mut *mut libc::c_void,
        ) -> libc::c_int;
        fn acl_get_tag_type(entry: *mut libc::c_void, tag: *mut libc::c_int) -> libc::c_int;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }
    // SAFETY: the fd is live; the returned ACL has acl_free ownership.
    let acl = unsafe { acl_get_fd_np(directory.as_raw_fd(), 0x100) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        // Darwin returns ENOENT when this live directory has no extended ACL.
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(error)
        };
    }
    struct Acl(*mut libc::c_void);
    impl Drop for Acl {
        fn drop(&mut self) {
            // SAFETY: this is the ACL allocation returned above.
            unsafe {
                acl_free(self.0);
            }
        }
    }
    let acl = Acl(acl);
    let mut which = 0; // ACL_FIRST_ENTRY; Darwin returns EINVAL at end.
    loop {
        let mut entry = std::ptr::null_mut();
        // SAFETY: ACL and out pointer are valid.
        if unsafe { acl_get_entry(acl.0, which, &mut entry) } < 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::EINVAL) {
                Ok(())
            } else {
                Err(error)
            };
        }
        let mut tag = 0;
        // SAFETY: entry belongs to the live ACL, tag is a valid out pointer.
        if unsafe { acl_get_tag_type(entry, &mut tag) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if tag == 1 {
            return Err(denied("Unix listener ancestry must not contain macOS ACL allow entries; use a private directory with non-granting ancestry"));
        }
        which = -1; // ACL_NEXT_ENTRY
    }
}
