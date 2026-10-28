# Unix Stream Sockets (macOS and Linux)

TCP capabilities in `locho` also authorize raw byte-stream services backed by a
filesystem Unix stream socket on macOS and Linux. Listener and upstream choices
are independent: TCP→TCP, TCP→Unix, Unix→TCP, and Unix→Unix all use the same
`locho/3` wire protocol and capability format (`<service>:tcp:<secret>`).

Unix paths never travel across the network, wire protocol, or portable tickets;
they are strictly local policy on the host or attachment machine.

## Host Configuration (Unix Upstream)

To expose a local Unix stream socket on a host, set `type = "tcp"` and replace
`endpoint` with `unix_socket` (do not combine both fields on the same service):

```toml
# locho.toml
[[services]]
name = "workspace"
type = "tcp"
unix_socket = "/home/alice/.config/example/api.sock"
# Optional raw-stream idle timeout in seconds.
# Omit for 300 seconds; set to 0 to disable byte-idle expiry.
tcp_idle_timeout_secs = 0
```

Start (or restart) the host with the configuration:

```sh
locho host --config locho.toml
```

Generate the attachment capability as usual:

```sh
locho share workspace --config locho.toml
```

## Attachment Configuration (Unix Listener)

### Single-Service CLI Attachment

Prepare an existing private parent directory owned by the effective user (mode
`0700`), then attach using `--listen-unix`:

```sh
mkdir -m 700 -p "$HOME/.local/run/locho"
locho attach '<host-id>' 'workspace:tcp:<secret>' \
  --listen-unix "$HOME/.local/run/locho/api.sock" \
  --tcp-idle-timeout-secs 0
```

Alternatively, a TCP capability backed by a Unix upstream on the host can still
be attached to an ordinary local TCP port (`--listen 127.0.0.1:9000`), and vice
versa.

### Multi-Service Attachment File (`attachments.toml`)

In `attachments.toml`, replace `listen_port` with `listen_unix` for any TCP
capability that should listen on a local Unix socket (exactly one listener field
is required per service):

```toml
# attachments.toml
host_id = "<host-id>"

[[services]]
capability = "workspace:tcp:<secret>"
listen_unix = "/home/bob/.local/run/locho/workspace.sock"
tcp_idle_timeout_secs = 0

[[services]]
capability = "database:tcp:<secret>"
listen_port = 5432
```

Start all configured listeners over one shared peer connection:

```sh
locho attach --config attachments.toml
```

## Path Constraints and Platform Support

- **Supported OSes**: macOS and Linux only. Windows explicitly rejects Unix
  upstreams and listeners while keeping ordinary TCP attachments supported.
- **Absolute paths required**: Paths must be absolute, with no `~` or
  environment variable expansion inside TOML.
- **Length limits**: The maximum configured path length is **103 bytes on
  macOS** and **107 bytes on Linux**. Keep directory and socket basenames short
  (Linux directory-relative binding through `/proc/self/fd` may impose a shorter
  basename limit, and `/proc/self/fd` must be available on Linux).
- **Protocol scope**: Only filesystem stream sockets (`SOCK_STREAM`) are
  supported. HTTP capabilities do not accept Unix listeners or raw-stream idle
  options. Datagram sockets, Linux abstract sockets, Windows named pipes, file
  descriptor passing (`SCM_RIGHTS`), and ancillary data are unsupported.
- **SDK boundary**: The public native (`locho-client-native`), Flutter
  (`locho_flutter`), and browser (`locho_web`) SDK APIs remain TCP loopback /
  HTTP-subset APIs and do not expose Unix listener handles.

## Idle Policy and Resource Budgets

- **Idle timeout (`tcp_idle_timeout_secs`)**: Local to each end. Omitted
  defaults to `300` seconds (5 minutes), `0` disables byte-idle expiry, and
  positive values range from `1` to `3,153,600,000` seconds.
- **Stricter endpoint wins**: Both the host service and the attachment must set
  `tcp_idle_timeout_secs = 0` for indefinite application-byte silence. Older
  peers still expire after 5 minutes. Transport keepalives do not reset the
  application-byte deadline.
- **Connection budgets**: At most 128 raw-stream connections are active per host
  and per attachment process. Budgets remain enforced when idle expiry is
  disabled, so silent clients can consume capacity until closed.

## Filesystem Security and Lifecycle

- **Parent directory and ancestry**: Unix attachment listeners require an
  existing private parent directory owned by the effective user (mode `0700`, no
  group/other access). Listener operations use pinned directory handles; unsafe
  writable ancestry and non-root parent symlinks are refused, and macOS ACL
  `allow` entries in listener ancestry are conservatively rejected.
- **Socket creation and cleanup**: Listeners are created with mode `0600` before
  accepting connections. Existing paths (including stale sockets) are refused
  rather than unlinked automatically. On normal shutdown (`SIGINT`/`Ctrl-C` or
  `SIGTERM`), the attachment removes only the socket it created after verifying
  device/inode identity (best-effort against concurrent same-UID changes).
- **Crash recovery**: A crash or `SIGKILL` can leave a stale socket path on
  disk. Verify and remove stale socket files manually before restarting the
  attachment.
- **Peer credentials**: The upstream service sees the `locho` host process as
  its local peer, not the original client's `UID`/`GID`/`PID`.

See [Unix socket security in SECURITY.md](../SECURITY.md#unix-socket-attachments)
for the complete filesystem trust model, and the
[native Unix stream test harness](../tests/perf/README.md#native-unix-stream-smoke-stress-and-soak-tests)
for smoke, stress, and soak verification commands.
