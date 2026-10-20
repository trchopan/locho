# Security Policy

## Supported Versions

The currently supported release is `1.1.1`. Older releases and
development snapshots are not supported; fixes may be backported at maintainer
discretion.

## Reporting a Vulnerability

Please do not open a public issue for an undisclosed vulnerability.

GitHub Private Vulnerability Reporting is enabled for this repository. Use
[GitHub Private Vulnerability Reporting](https://github.com/trchopan/locho/security/advisories/new)
to report vulnerabilities privately. Do not disclose the issue publicly until
a fix or coordinated disclosure plan is available.

Include the following where possible:

- A clear description and impact assessment.
- Reproduction steps or a proof of concept.
- Affected versions, platforms, and configurations.
- Any suggested mitigation or fix.

## Unix socket attachments

Unix stream paths are local policy only. A TCP capability can select a configured
Unix upstream, and a TCP attachment can expose a configured Unix listener, but
the path is never supplied by a remote request or carried in a ticket. Listener
parents must already exist, be owned by the effective user, and have no group or
other access (normally mode `0700`). Directory operations are anchored to an open
parent handle. Ancestry must be owned by root or the effective user and cannot be
group/other writable unless sticky-directory protections apply. Non-root parent
symlinks are refused; root-owned system aliases such as macOS `/tmp` are supported.
macOS ACL allow entries in listener ancestry are conservatively rejected because
ACL grants can override mode bits. Linux uses `/proc/self/fd` for anchored binding
and permission changes. macOS binds on a short-lived thread using its private cwd;
the process cwd and umask are never changed.

Locho creates listener entries mode `0600` before accepting, refuses existing
entries, and checks device/inode identity before directory-relative cleanup.
Root and same-UID processes are trusted: POSIX has no portable atomic
compare-inode-and-unlink operation, so another process running as this UID must
not concurrently replace listener entries or change directory permissions/ACLs.
Replacement preservation is best-effort against such same-UID races, not an
absolute guarantee. Locho does not
preserve the caller's UID/GID/PID credentials: the upstream sees the Locho host
process as its local peer.

`SIGKILL` and crashes can leave stale socket paths. Operators should verify and
remove those paths explicitly; Locho never automatically replaces an existing
listener or chmods a symlink target. Unix socket support is limited to filesystem stream sockets on macOS and
Linux; datagrams, abstract sockets, named pipes, descriptor passing, and ancillary
data are not supported.

Response and remediation timelines are best-effort. This project is maintained
as open source, so response speed depends on maintainer availability and the
complexity of the issue.
