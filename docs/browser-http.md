# Browser buffered HTTP subset

The prerelease browser SDK transports HTTP/1.1 bytes through an authorized Locho
TCP stream; it does not call `window.fetch` for the homelab API. One stream is opened
per request, with no pooling, pipelining or automatic replay. The peer session is
reused. This API requires a parsed TCP ticket.

```ts
const ticket = await LochoTicket.parse(ticketText);
const client = await LochoClient.connect({ ticket, http: { authority: 'sample-api' } });
try {
  const response = await client.fetch('/api/notes', {
    headers: { Authorization: `Bearer ${token}` },
    signal,
    timeoutMs: 15_000,
  });
  if (!response.ok) { /* backend HTTP status, not a Locho handshake error */ }
  const notes = await response.json();
} finally { await client.close(); }
```

## Supported inputs

- `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE`, `OPTIONS`; default `GET`.
- Root-relative ASCII paths with query strings. Percent-encode non-ASCII paths.
  Absolute/network-path URLs, fragments, backslashes, spaces and control characters
  are rejected. Paths are not automatically normalized or redirected.
- `HeadersInit` application headers. Values must be ASCII; CR/LF is rejected.
- Bodies: string (UTF-8), `Uint8Array`, `ArrayBuffer`; absent body is empty.
  GET/HEAD bodies are rejected. JSON serialization and Content-Type are caller-owned.
- Logical authority configured at connect, default `locho.local`; it is not the
  website origin or relay URL. It is validated before sending each request.
- `AbortSignal` and integer `timeoutMs` in 1–300000, default 15000.

Only these request options are supported. Fetch options such as `credentials`,
`redirect`, `cache`, `mode`, `integrity`, `keepalive` and upload streams are rejected.
FormData, Blob, Request objects, cookies and automatic JSON serialization are not
implemented. The existing `health` proof remains available for compatibility.

Caller framing/hop-by-hop headers are rejected: Host, Content-Length,
Transfer-Encoding, Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization,
Proxy-Connection, TE, Trailer, Upgrade, Expect, Accept-Encoding. SDK-generated
headers include `Content-Length`, `Connection: close`, `Accept-Encoding: identity`.

## Response framing and limits

- `httparse` validates bounded HTTP/1.1 status/header blocks (HTTP/1.0 is unsupported).
- Content-length, chunked and connection-close bodies are buffered as bytes.
  Content-length/chunked completion does not wait for EOF. The stream is then dropped.
- Up to eight informational responses are accepted before the final response.
  HTTP 101 is unsupported. HEAD and 204/205/304 responses have null Response bodies.
- TE+CL, conflicting/malformed lengths, invalid/truncated chunks or bodies, and
  non-identity Content-Encoding fail with sanitized SDK errors.
- Chunk extensions and nonempty trailer sections are explicitly unsupported in this
  initial subset. Trailer values are never silently promoted into response headers.
- Limits: 4 MiB request body, 8 MiB decoded response body, 64 KiB aggregate response
  headers, 100 aggregate response fields (including informational blocks). Request
  headers include at most 100 fields and 64 KiB. Chunk-size framing has a separate
  aggregate 64 KiB limit. HTTP operations are capped at 16 per client without queuing.
- The timeout covers reconnect acquisition, authorization, writing and full response
  buffering. Abort drops only this request stream; close cancels all owned work.
  Request data may already have reached the backend when abort/timeout occurs.

Backend 4xx/5xx and redirects remain ordinary responses. No redirect is followed.
Handshake/transport/framing failures reject with `LochoError`; no backend status is
fabricated. Excess concurrent HTTP operations reject with `resourceLimit`.

## Standard Response limitations

The result is a synthetic buffered `Response`. `url` is empty, `redirected` is false,
`type` follows the platform constructor rather than a browser network fetch, and
statusText is not populated. Repeated application headers are appended using
standard Headers semantics; Set-Cookie has no browser cookie-jar effect. Hop-by-hop,
Connection-nominated fields and upstream Content-Length are removed after decoding.
Header values outside ASCII are currently rejected. CORS, cache behavior, native
HTTP authentication and browser cookie management are not inherited.

## Evidence

Local production WASM tests on macOS pass 48 cases across Chromium, Firefox and
WebKit, using the official N0 relay and real CLI host. HTTP coverage includes a
pinned reference notes API checkout (`locho_sdk_examples` commit
`310d6cf756c1b1d8ccd584c576c94f97e9c72a94`), health/login/CRUD/logout, binary echo,
chunked responses, header preservation, redirects, bodyless results, cancellation,
deadline and concurrency enforcement. Rust tests exercise fragmented headers,
completion before EOF, ambiguous/truncated framing and header/body limits.

For local full integration, build the examples API and set `LOCHO_REFERENCE_API`
to its checkout alongside the existing N0 fixture environment variables. CI checks
out and builds that exact backend commit. Hosted Linux results are a separate gate.
This milestone does not establish npm/tarball consumption, real mobile-browser
support, general status subscriptions, streaming/SSE or WebSockets.
