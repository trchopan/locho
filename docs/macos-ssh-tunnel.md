# macOS SSH Tunnel Hardening

SSH over a Locho TCP attachment benefits from an explicit idle policy, enough
process descriptors, and a recovery path independent of the tunnel. This guide
describes configuration principles, not an installer or automatic migration.

## Separate critical attachments

If SSH is your administrative access path, consider running it in a separate
`locho attach` process from other services. A shared process still shares its
descriptor limit and lifecycle; restarting it interrupts every attached service.
Separate processes reduce that shared failure boundary but do not remove shared
network or dependency failures.

For example, put SSH alone in `~/.config/locho/attachments-ssh.toml`:

```toml
host_id = "<host-id>"
listen_host = "127.0.0.1"

[[services]]
capability = "ssh:tcp:<secret>"
listen_port = 2222
tcp_idle_timeout_secs = 0
```

Keep real attachment files private (`chmod 600`); capability tokens are bearer
credentials. Do not put them in Git, launch-agent arguments, logs, or bug reports.
Bind the local SSH attachment to loopback unless broader access is intentional.

## Set the SSH idle policy at both ends

On the host, disable application-byte idle expiry for SSH only:

```toml
[[services]]
name = "ssh"
type = "tcp"
endpoint = "127.0.0.1:22"
tcp_idle_timeout_secs = 0
```

The default TCP byte-idle timeout is 300 seconds. Setting it to `0` on both the
host service and client attachment permits quiet SSH sessions. Keep other service
policies unchanged. An attachment alone cannot override the host's timeout.
See [Unix stream sockets and idle policy](unix-sockets.md).

Configure SSH's own failure detection in `~/.ssh/config`:

```sshconfig
Host lab-ssh
    HostName localhost
    Port 2222
    User alice
    ServerAliveInterval 30
    ServerAliveCountMax 3
    ConnectTimeout 10
```

Keep these settings inside the intended `Host` stanza, before any subsequent
`Host` or `Match` boundary. Keepalives detect failures; they do not resume a stream
after transport loss. They also generate traffic that can mask byte-idle expiry.
Disabling expiry allows idle connections to hold resources longer, so monitor
connection and descriptor usage.

## Configure per-process launchd limits

For a user LaunchAgent, merge suitable limits into its plist rather than replacing
unrelated settings. An illustrative starting point is:

```xml
<key>SoftResourceLimits</key>
<dict>
    <key>NumberOfFiles</key>
    <integer>1024</integer>
</dict>
<key>HardResourceLimits</key>
<dict>
    <key>NumberOfFiles</key>
    <integer>4096</integer>
</dict>
```

Choose limits for your workload and verify the actual inherited limit after an
approved restart. A shell's `ulimit -n` does not establish a LaunchAgent's limit.
The CLI logs an estimated attachment descriptor budget at startup on Unix. Its
connection budget is not an exact descriptor count: discovery, relay connections,
DNS, logs, and runtime resources add variable overhead.

Current source retains TCP/Unix and HTTP attachment listeners across transient
accept errors, including descriptor exhaustion, using bounded, cancellable
backoff. This permits recovery when resources are released; it does not fix leaks,
guarantee new connections during exhaustion, or prevent other components from
failing. The official `1.3.0` binary predates this listener-recovery change.

## Upgrade and rollback safely

Before changing a remote access service:

1. Verify an independent backup access path reaches the expected host. Do not
   restart that backup while relying on it for recovery.
2. Record the installed binary's version/checksum and which agents are loaded.
   Verify the candidate archive against its release checksum, or clearly identify
   a locally patched build. Never present a dirty build as an official artifact.
3. Preserve the original executable and affected configurations in a private
   recovery directory. Record file modes and newly created files. Backups can
   contain secrets and must not be committed or shared.
4. Review the precise configuration and restart scope. Keep host identity and
   service capabilities unchanged; connectivity recovery is not a reason to rotate
   credentials.
5. Restart only the approved Locho agents, accounting for interrupted streams.
   Verify their registrations, running PIDs, local listeners, and inherited limits.

For rollback, stop the affected agents, restore only the recorded files and modes,
remove only files created by that change, and restore the original loaded/unloaded
agent state. Verify access afterwards; a successful registration alone does not
prove the tunnel works.

## Verify recovery and quiet sessions

- Confirm SSH and non-SSH attachments run in separate PIDs if isolation is intended.
- Test primary SSH and the independent backup against the expected host.
- Test a quiet SSH session for longer than five minutes with
  `ssh -o ServerAliveInterval=0 lab-ssh`, then run a command. This prevents SSH
  keepalive traffic from hiding idle expiry.
- Compare descriptor samples within one PID lifetime and check that counts return
  toward baseline after work drains. Higher limits alone do not demonstrate leak
  freedom.
- Review new errors after restarts and network transitions. Sanitize diagnostics
  before sharing; never paste configurations, environments, keys, or capabilities.

For DNS deadlines, see [CLI DNS recovery](dns-recovery.md). DNS racing and listener
recovery do not establish crash-free operation. Reports involving macOS's
`SCNetworkInterfaceGetLocalizedDisplayName` during transitive `netdev` interface
enumeration require separate dependency investigation and network-transition
testing; these changes do not remove that call path.
