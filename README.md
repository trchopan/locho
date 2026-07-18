# locho

`locho` is an HTTP reverse proxy over an [iroh](https://iroh.computer/) tunnel.
Run a host next to an HTTPS upstream, then attach from another machine and
access that upstream through a local HTTP proxy.

## How it works

```text
  Client machine                              Host machine
+------------------+                    +------------------+
| curl / HTTP app  |                    | locho host       |
+--------+---------+                    |                  |
         |                              |  HTTPS upstream  |
         v                              |        ^         |
+------------------+     iroh tunnel    |        |         |
| locho attach     |===================>|  request proxy   |
| 127.0.0.1:8765   |  auth + HTTP data  +--------+---------+
+------------------+                             |
                                                 v
                                          +--------------+
                                          | https://...  |
                                          +--------------+

          attach uses the host ID and secret printed by `locho host`
```

## Requirements

- Rust stable with Cargo
- Network access for iroh node discovery and relay connectivity

## Build and test

```sh
cargo build --release
cargo test
```

The release binary is written to `target/release/locho`. Run it directly with
`./target/release/locho`, or copy it to a directory on your `PATH` to use the
`locho` commands below.

## Usage

Start a host and point it at an HTTPS upstream:

```sh
locho host --upstream https://example.com
```

The host prints an attach command containing its persistent node ID and attach
secret. Run that command on another machine:

```sh
locho attach <host-id> <secret>
```

The local proxy listens on `127.0.0.1:8765` by default. Change the address
with `--listen`:

```sh
locho attach <host-id> <secret> --listen 127.0.0.1:9000
```

Then send requests to the local proxy:

```sh
curl http://127.0.0.1:8765/path
```

Host identity and attach credentials are stored in:

```sh
~/.local/share/locho/
```

The `host.key` file contains the persistent iroh identity and
`host_state.json` contains the attach secret. Set `LOCHO_STATE_DIR` to use an
alternate directory for development or testing.

Rotate an attach secret after it has been exposed. Stop the host first, then
run:

```sh
locho rotate-secret
```

Identity reset and secret rotation fail while a host is running.

This preserves the host ID and invalidates the previous attach secret. To
replace the host identity as well, stop the host and run:

```sh
locho reset-identity
```

The next `locho host` start will generate a new host ID and attach secret.

## Security notes

- The attach secret authorizes requests and should be treated as a credential.
- Rotate the attach secret if it leaks; reset the identity if the private host
  key leaks.
- Do not commit secrets or expose the local proxy beyond the intended machine.
- Only HTTPS upstream URLs are accepted.
- Request and response bodies are limited to 32 MiB.

## License

Licensed under the MIT License. See [LICENSE](LICENSE).
