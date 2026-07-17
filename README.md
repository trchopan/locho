# locho

`locho` is an HTTP reverse proxy over an [iroh](https://iroh.computer/) tunnel.
Run a host next to an HTTPS upstream, then attach from another machine and
access that upstream through a local HTTP proxy.

## Requirements

- Rust stable with Cargo
- Network access for iroh node discovery and relay connectivity

## Build and test

```sh
cargo build --release
cargo test
```

## Usage

Start a host and point it at an HTTPS upstream:

```sh
cargo run -- host --upstream https://example.com
```

The host prints an attach command containing its node ID and a generated
secret. Run that command on another machine:

```sh
cargo run -- attach <host-id> <secret>
```

The local proxy listens on `127.0.0.1:8765` by default. Change the address
with `--listen`:

```sh
cargo run -- attach <host-id> <secret> --listen 127.0.0.1:9000
```

Then send requests to the local proxy:

```sh
curl http://127.0.0.1:8765/path
```

A reusable secret can be supplied to the host with `--secret`:

```sh
cargo run -- host --upstream https://example.com --secret '<secret>'
```

## Security notes

- The attach secret authorizes requests and should be treated as a credential.
- Do not commit secrets or expose the local proxy beyond the intended machine.
- Only HTTPS upstream URLs are accepted.
- Request and response bodies are limited to 32 MiB.

## License

Licensed under the MIT License. See [LICENSE](LICENSE).
