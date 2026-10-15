# Portable single-service tickets v1

A ticket is an explicit bearer credential for one service on one host. It packages
existing capability material; it does not introduce a new authorization scheme,
expiry, signature, encryption, or wire protocol version. `locho/3` is unchanged.
Anyone possessing a ticket can use its capability until that service secret is rotated.

## Encoding

`locho1:` followed by unpadded RFC 4648 URL-safe base64 of a UTF-8 JSON object:

```json
{
  "host_id": "64 hexadecimal characters",
  "capability": "sample-api:tcp:secret",
  "peer_relay_urls": ["https://relay.example.org/"]
}
```

All three fields are required. `peer_relay_urls` can be empty. The prefix carries
the ticket format version, independently of the transport protocol. Unknown
versions, unknown/duplicate fields, padding, invalid base64, trailing non-JSON data,
and surrounding whitespace are rejected. JSON field order and internal whitespace
are accepted; encoding produces compact JSON in the order shown above.

Limits and normalization:

- At most 8192 bytes for the complete encoded ticket.
- Host identity: exactly 64 hexadecimal characters, normalized to lowercase.
  This is syntactic validation; endpoint identity authentication occurs on connection.
- Capability: existing safe service name and `http` or `tcp` type, with a nonempty
  printable ASCII secret of at most 1024 bytes, excluding the capability separator `:`.
  Ticket validation is narrower than legacy capability parsing.
- At most four ordered peer relay hints, each at most 512 bytes before parsing.
- Relay hints must be HTTPS base URLs with a host, no userinfo, query, fragment,
  or non-root path. Whitespace/control characters are rejected. URL normalization
  removes a trailing DNS root dot; normalized duplicates are rejected. Explicit
  ports and IP literal hosts are supported.

Relay hints are non-authoritative address hints. They do not select client relay
policy, carry relay authentication tokens, disable discovery, or weaken host identity
verification. An unreachable hint can be superseded by discovery. Live relay
migration and custom authenticated-relay behavior remain separate integration gates.
Consumers must not replay application requests to recover addressing.

## CLI

```sh
locho share sample-api --config locho.toml --ticket
locho share sample-api --config locho.toml --ticket \
  --peer-relay-url https://relay.example.org/
```

The host must already have initialized its identity and service secret. Output is
one ticket plus a newline. Hints are supplied explicitly; `share` does not probe the
network. Repeat `--peer-relay-url` for additional hints. Ticket output conflicts with
`--relay-config` and `--direct-address`: client policy and native direct addresses
are not represented in v1. Ordinary `share` output remains an attachment command.

## Shared implementation and handling

`locho_core::ticket::{Ticket, parse, TicketError}` owns encoding and validation
without the transport feature. Construction and parsing validate the same fields.
`Ticket::encode()` is an explicit credential-export operation; `Debug` is redacted
and errors contain fixed categories rather than supplied values. A ticket has no
implicit `Display` or public serde serialization.

Do not put ticket strings in browser URLs, logs or diagnostic reports. Base64 is
reversible. Shared interoperability vectors with synthetic credentials live in
`crates/locho-core/tests/vectors/tickets-v1.json`.

## SDK consumption

Both SDKs parse through the shared Rust codec and expose redacted ticket objects,
non-secret host/service metadata, and explicit canonical export. HTTP tickets are
valid documents, but initial TCP attachment APIs reject them with `unsupportedService`.
Mixed ticket and explicit identity/capability input is rejected with `conflictingOptions`.
Malformed ticket input reports `invalidTicket` without echoing the supplied value.
Neither SDK trims input: UI applications may trim outer clipboard whitespace explicitly.

Browser:
```ts
const ticket = await LochoTicket.parse(text);
const client = await LochoClient.connect({ ticket, signal });
try {
  const response = await client.health(); // capability retained by the ticket
} finally { await client.close(); }
```

Flutter:
```dart
final ticket = await LochoTicket.parse(text);
final client = await LochoClient.create();
try {
  final attachment = await client.attach(ticket: ticket);
  // Ordinary HTTP clients use attachment.httpBaseUri.
} finally { await client.close(); }
```

Native Rust uses `TcpOptions::from_ticket(&ticket)`. Native/Flutter may additionally
set a direct address for development; this adds a transport candidate alongside
ticket relay hints. Client relay policy remains independent. Browser ticket input
cannot be mixed with the existing explicit `peerRelayUrl` option. All ticket hints
are forwarded to iroh; transport address sets do not guarantee array dialing order.

These are prerelease source APIs. Standalone package distribution, general browser
HTTP requests, physical-device ticket acceptance and authenticated-relay validation
remain separate delivery gates.
