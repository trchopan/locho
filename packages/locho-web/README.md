# Locho Browser Connectivity Proof

Unpublished browser WASM proof package. It currently validates only host peer
connectivity and one raw HTTP health response through the existing TCP capability.
It is not the final Fetch-like SDK and does not yet implement portable tickets,
streaming bodies, redirects, browser storage, WebSockets or custom relay auth.

## Build

Requirements: Rust 1.96+, `wasm32-unknown-unknown`, a browser-compatible Clang,
`wasm-bindgen-cli` matching the generated wasm-bindgen dependency, Node/npm.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126 --locked
npm install
npm run build:wasm
npm run build
npm run build:production
npx playwright install chromium firefox webkit
npm run test:browser
npm run dev
```

Apple Clang lacks a wasm C target for ring; use the controlled Linux CI runner
or a WebAssembly-capable LLVM installation. For Homebrew LLVM, build with
`CC_wasm32_unknown_unknown=/opt/homebrew/opt/llvm/bin/clang npm run build:wasm`.
Do not claim browser feasibility from native builds. CI records the actual browser
build result, generated artifact versions and integration evidence.

## Proof API

```ts
const client = await LochoClient.connect({ hostId, peerRelayUrl });
const response = await client.health(capability, 'sample-api');
console.log(response.status, await response.text());
await client.close();
```

Alternatively, import `LochoTicket` and use
`const ticket = await LochoTicket.parse(text); const client = await ticket.connect(signal);`
then `await client.health()` without a capability argument. The ticket keeps the
selected capability in Rust; metadata exposes only host/service/type/relay hints.
`ticket.export()` explicitly returns the credential; string/JSON formatting is
redacted. Mixed ticket and host/hint/capability inputs report `conflictingOptions`.
Valid HTTP tickets report `unsupportedService` on connect. All peer hints are passed
to iroh without a dialing-order guarantee. See [ticket v1](../../docs/tickets.md).

Local production WASM ticket verification passes 39 tests across Chromium, Firefox
and WebKit, including shared vectors and a real CLI-generated ticket over N0.
The ticket-backed buffered `client.fetch` API now supports JSON/text/byte requests.
See [the exact HTTP subset](../../docs/browser-http.md) for headers, framing, limits,
synthetic Response metadata and unsupported features. Standalone distribution and
status subscriptions remain follow-up milestones.

### Cancellation and close

`connect({ hostId, signal })` and `health(capability, authority, signal)` accept
an `AbortSignal`. Already-aborted calls reject with `LochoError.code === 'cancelled'`
before endpoint/request work begins. Startup cancellation reaches Rust and waits
for session cleanup before rejecting. WASM module initialization itself is not
abortable. Endpoint binding and peer connection each have a 15-second deadline;
session cleanup follows the shared core bounds.

Health cancellation drops only the operation's authorized stream. Concurrent
requests and the peer session remain usable. An active health operation interrupted
by `close()` rejects with `closed`. Calls after close starts also reject with
`closed`; repeated/concurrent close calls share the same completion promise.
Close retains the shared core's bounded cleanup and final status. Abort/close races
may resolve as `closed` when client shutdown wins. No application requests are
automatically replayed.

The interactive example aborts superseded operations and requests cleanup on
`pagehide`, and ignores stale results. Browsers do not guarantee asynchronous
cleanup finishes during page destruction; applications should explicitly await
close when their own navigation lifecycle allows it.

The optional peer relay URL is a host address hint, separate from client relay
policy. The proof requests `/health` with a TCP service handshake and returns a
standard `Response` assembled from the raw response. Application authority is
explicit; it is never derived from the website origin. Peer `connected` does not
mean capability/upstream/API readiness.

The API does not automatically retry or replay. Close on page lifecycle changes,
discard stale results after restoration, and keep credentials out of logs/URLs.
The minimal page is intentionally not a production authentication UI.

Playwright's default matrix verifies production asset loading in Chromium, Firefox
and WebKit. The controlled CI job additionally starts a real Locho host and local
TCP health backend, then uses the official N0 relay/discovery configuration for
the fixture. The addressing matrix uses fresh browser contexts for a correct
explicit hint, host-ID-only discovery, a dotted DNS hostname, and an unreachable
hint (`https://127.0.0.1:1/`). Every case asserts the relay path and exact health
response. It sets `LOCHO_N0_INTEGRATION=1`, `LOCHO_N0_FIXTURE_FILE` (a private
temporary path) and `LOCHO_HOST_BINARY`; setup derives credentials and cleans
up the host/backend processes. This is public-network evidence and can fail when
N0 discovery or relay service is unavailable.

The browser adapter normalizes a trailing DNS root dot in explicit hints and
verified N0 Pkarr lookup results before handing addresses to the transport. The
N0 publisher, signed-record verification, identity verification and startup deadline
are retained. iroh performs discovery alongside address hints; the adapter does not
retry or replay HTTP requests. WebSocket/error counts are attached as sanitized
test diagnostics without retaining URLs, headers or frames.

Local macOS production WASM verification on 2026-10-15 passed all 18 cases in
Chromium, Firefox and WebKit. This includes recovery from an unreachable hint;
it does not simulate migration between two live relays or an authenticated relay.
PR #89 also passed hosted Linux verification for the addressing matrix.

The lifecycle follow-up adds a production test entry point and controllable upstream
fixture. Local macOS production WASM verification passed 33 tests across all three
browsers: startup/pre-abort cancellation, independent concurrent traffic, stalled
stream cancellation, active close, upstream interruption/unavailability and repeated
close cycles. Tests observe upstream connection release and request counts; these
are not heap-leak measurements. Hosted Linux lifecycle verification is pending.

## Feasibility evidence

Required before freezing browser tickets/API:

- Controlled browser relay/TLS connection to a real Locho host through the official N0 relay.
- Host-ID discovery, explicit peer hint, stale-hint fallback behavior.
- Chromium, Firefox, WebKit and mobile-browser smoke.
- Wrong capability, stopped upstream, interruption, cancellation and close cleanup.
- Custom authenticated relay support verified or marked deferred.

The shared `locho/3` framing/authorization code is reused. Browser runtime, relay,
crypto and discovery behavior remain target-specific evidence.
