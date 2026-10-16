# Browser HTTP subset

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

## Streaming HTTP — browser package dev.3

`fetchStream(path, options)` resolves as soon as final headers have been parsed.
It accepts the same method/headers/body/signal as `fetch`, replacing `timeoutMs`
with `setupTimeoutMs` (default 15000) and `idleTimeoutMs` (default 75000). Both are
integers in 1–300000 ms. Unknown options, including `timeoutMs`, are rejected.

```ts
const abort = new AbortController();
const response = await client.fetchStream('/api/events', {
  headers: { Authorization: `Bearer ${token}` },
  signal: abort.signal,
  setupTimeoutMs: 15_000,
  idleTimeoutMs: 75_000,
});
if (!response.ok) {
  await response.body?.cancel();
  // Handle HTTP status (including 401/429) before attempting SSE parsing.
} else {
  const reader = response.body!.getReader();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      // Feed bytes to an incremental bounded application parser.
      consume(value);
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
```

- Setup covers peer acquisition, service authorization, request write and final
  headers. It does not cover the body lifetime. Deadline failure is `setupTimeout`;
  an underlying service/session timeout can still report `timeout`.
- Idle applies only while waiting for upstream bytes during an active pull,
  including chunk framing. Every successful upstream read resets that wait.
  Consumer pauses do not run an idle timer. A silent pending read fails with
  `idleTimeout`. There is no automatic reconnect, lifetime deadline or replay.
- Rust retains an 8 KiB BufReader, at most one 16 KiB decoded read, and bounded
  framing/header storage. JS requests no read-ahead (`highWaterMark: 0`); one pull
  is in flight. Transport receive windows/runtime buffers are additional bounds,
  not included in this decoder memory budget. Server HTTP chunks can be larger
  than 16 KiB; they are read in bounded pieces, never allocated as whole chunks.
- Streamed bodies have no cumulative body/chunk-count limit. Chunk-size lines are
  capped at 1024 bytes each; trailer handling remains bounded/unsupported as above.
  Buffered fetch retains its 8 MiB body and aggregate 64 KiB chunk-size limits.
- The shared 16-operation cap counts buffered requests and live streamed bodies.
  A body owns its permit and authorized stream until framed completion, EOF,
  error, cancel or client close. An unread body retains its permit; callers must
  consume or cancel it. `releaseLock()` alone does not cancel. No pooling is added.
- Abort remains attached after headers and cancels only that response. `reader.cancel()`
  waits for Rust body cleanup. Client close cancels even unread/paused bodies and
  awaits their cleanup along with shared session close. HEAD/204/205/304 bodies
  remain null and immediately release ownership.
- Backend errors/redirects remain ordinary Responses; redirects are not followed.
  Framing/transport failures after headers error the body rather than fabricate an
  HTTP status. Direct `reader.read()` rejects with sanitized `LochoError` codes;
  platform convenience methods (`text`, `json`, `arrayBuffer`) may replace a stream
  error with a browser-specific TypeError/DOMException. Do not rely on their codes.
- The stream is a default byte-chunk ReadableStream, not a BYOB byte source.
  Chunks are arbitrary boundaries, not HTTP chunks, UTF-8 characters or SSE frames.
  `Response.clone()`/`body.tee()` can queue unbounded data for a slower branch;
  they are outside this bounded single-reader usage contract. Likewise convenience
  whole-body methods buffer by design and are unsuitable for infinite SSE.

This API transports bytes. SSE framing, schema validation, heartbeat/reconnect policy,
session validation and canonical refresh belong to the consuming application.

## Standard Response limitations

The result is a synthetic `Response`. `url` is empty, `redirected` is false,
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
That original buffered milestone did not establish streaming. The dev.3 follow-up
adds Rust tests for headers/partial chunks before EOF, reads beyond the buffered
body limit, and per-line rather than lifetime chunk framing bounds. Production
WASM/browser tests cover incremental data, deadlines, paused readers, abort/cancel,
operation capacity, pending/unread close and authenticated note invalidations
against examples commit `75a93b3f50900becc320d50c2ecf2dc662292141` (OpenAPI 1.1.0).
The same matrix runs against an installed tarball in the external Vite consumer.
Local macOS verification passes all 69 external-consumer cases across Chromium,
Firefox and WebKit, plus the locked Rust workspace suite (14 locho-web tests).
Physical/mobile browsers, app event consumers and WebSockets remain subsequent gates.
