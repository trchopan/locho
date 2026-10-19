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
parents must already exist, be owned by the effective user, and be mode `0700`.
Locho creates listener entries mode `0600`, refuses existing entries, and removes
only a socket whose device/inode still matches the entry it created. It does not
preserve the caller's UID/GID/PID credentials: the upstream sees the Locho host
process as its local peer.

`SIGKILL` and crashes can leave stale socket paths. Operators should verify and
remove those paths explicitly; Locho never blindly unlinks or follows an existing
path. Unix socket support is limited to filesystem stream sockets on macOS and
Linux; datagrams, abstract sockets, named pipes, descriptor passing, and ancillary
data are not supported.

Response and remediation timelines are best-effort. This project is maintained
as open source, so response speed depends on maintainer availability and the
complexity of the issue.
