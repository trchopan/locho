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

The current development machine's Apple Clang lacks a wasm C target for ring;
use the controlled Linux CI runner or a WebAssembly-capable LLVM installation.
Do not claim browser feasibility from native builds. CI records the actual browser
build result, generated artifact versions and integration evidence.

## Proof API

```ts
const client = await LochoClient.connect({ hostId, peerRelayUrl });
const response = await client.health(capability, 'sample-api');
console.log(response.status, await response.text());
await client.close();
```

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
the browser request. It sets `LOCHO_N0_INTEGRATION=1` and
`LOCHO_HOST_BINARY`; the global setup derives temporary credentials and cleans
up the host/backend processes. This is public-network evidence and can fail when
N0 discovery or relay service is unavailable.

## Feasibility evidence

Required before freezing browser tickets/API:

- Controlled browser relay/TLS connection to a real Locho host through the official N0 relay.
- Host-ID discovery, explicit peer hint, stale-hint fallback behavior.
- Chromium, Firefox, WebKit and mobile-browser smoke.
- Wrong capability, stopped upstream, interruption, cancellation and close cleanup.
- Custom authenticated relay support verified or marked deferred.

The shared `locho/3` framing/authorization code is reused. Browser runtime, relay,
crypto and discovery behavior remain target-specific evidence.
