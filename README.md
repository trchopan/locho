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

The host prints an attach command containing its node ID and a generated
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

A reusable secret can be supplied to the host with `--secret`:

```sh
locho host --upstream https://example.com --secret '<secret>'
```

## Security notes

- The attach secret authorizes requests and should be treated as a credential.
- Do not commit secrets or expose the local proxy beyond the intended machine.
- Only HTTPS upstream URLs are accepted.
- Request and response bodies are limited to 32 MiB.

## License

Licensed under the MIT License. See [LICENSE](LICENSE).
