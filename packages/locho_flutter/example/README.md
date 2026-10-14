# Foreground smoke app

See the parent package README for toolchains, native builds and device harness.
Run the shared notes API and Locho host, then paste host ID and TCP capability from
`locho share sample-api --config deploy/locho.toml`. The optional direct address is
the Locho host's reachable UDP bind address, not the upstream API TCP address.

Connect shows peer state/path/local port and explicitly requests `/health`. Repeat
GET without recreating the tunnel. Disconnect closes ownership. Hidden/paused/
detached triggers best-effort teardown; resumed recreates a desired active session
with generation checks and a fresh local URI. Inactive alone does not close it.

Memory-only: no credentials or settings persist. The status confirms peer
connectivity, not API/user authentication. Normal HTTP errors remain application
results, and tunnel socket closure does not uniquely identify an auth failure.
