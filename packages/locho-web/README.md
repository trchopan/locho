# Locho for the Browser

Connect a browser app to a private HTTP API through an encrypted Locho tunnel.
The API stays private, and users do not need to install the Locho CLI on their
device.

Use the SDK's fetch-like API for JSON requests and streaming responses. This is a
browser-only beta package; it does not replace the browser's native `window.fetch`.

## Install

```sh
npm install --save-exact locho_web@0.1.0-beta.2
```

The package includes ESM, TypeScript declarations and the WASM runtime. Consumer
apps do not need Rust, wasm-bindgen, a Locho checkout or install-time build scripts.
Node SSR and CommonJS are not supported because the SDK requires a browser runtime.

## Get A Ticket

Configure the private API as a TCP service on the machine where it runs:

```toml
[[services]]
name = "sample-api"
type = "tcp"
endpoint = "127.0.0.1:8080"
```

Start Locho and create a ticket:

```sh
locho host --config locho.toml
locho share sample-api --config locho.toml --ticket
```

Copy the complete `locho1:...` value into your application configuration or ticket
input. A ticket is a bearer credential for one service. Do not put it in a URL,
log, analytics event or source repository.

The CLI runs on the machine hosting the API. The browser user only needs your HTTPS
website. The ticket and your API's own login token are separate credentials.

## First Request

```ts
import { LochoClient, LochoTicket } from 'locho_web';

export async function checkApi(ticketText: string) {
  const ticket = await LochoTicket.parse(ticketText.trim());
  const client = await LochoClient.connect({
    ticket,
    http: { authority: 'sample-api' },
  });

  try {
    // Peer connectivity is not API readiness. Check the API explicitly.
    const response = await client.fetch('/health', { timeoutMs: 15_000 });
    if (!response.ok) {
      throw new Error(`API health check failed: ${response.status}`);
    }
    return await response.json();
  } finally {
    await client.close();
  }
}
```

`authority` is the logical HTTP `Host` value sent to the private API. It is not the
website hostname, ticket host ID or relay URL.

## JSON API Calls

Reuse one client for a foreground session. JSON encoding and API authentication are
application responsibilities:

```ts
const response = await client.fetch('/api/notes', {
  method: 'POST',
  headers: {
    Authorization: `Bearer ${sessionToken}`,
    'Content-Type': 'application/json',
  },
  body: JSON.stringify({ title: 'First note', body: 'Hello' }),
  signal: requestController.signal,
  timeoutMs: 15_000,
});

if (!response.ok) {
  // 4xx/5xx responses are ordinary backend HTTP responses.
  throw new Error(`API request failed: ${response.status}`);
}
const note = await response.json();
```

The initial HTTP API supports root-relative paths, common HTTP methods, JSON/text/
byte bodies, standard `Response` objects, redirects returned without following them,
bounded buffering and `AbortSignal`. It does not provide cookies, CORS behavior,
browser caching, native HTTP authentication, FormData or automatic JSON encoding.

## WASM And Vite

Vite normally discovers the WASM file through the package and emits it with your
application assets. If your deployment needs an explicit asset URL, initialize it
before parsing tickets or connecting:

```ts
import { initializeLocho } from 'locho_web';
import wasmUrl from 'locho_web/wasm?url';

await initializeLocho(wasmUrl);
```

Initialization is shared by concurrent callers. The first initialization input wins;
a failed initialization can be retried.

Deploy the website over HTTPS and serve `.wasm` with `Content-Type:
application/wasm`. A restrictive CSP needs to allow WASM compilation and Locho
discovery/relay connections, for example:

```text
script-src 'self' 'wasm-unsafe-eval'
connect-src 'self' https://dns.iroh.link https://*.iroh.link wss://*.iroh.link
```

Add only the origins required by your relay configuration. Custom CDN WASM assets
may also require CORS. No `SharedArrayBuffer` or cross-origin isolation headers are
required.

## Status And Lifecycle

Status observation is useful for showing connection progress and recovery:

```ts
const unsubscribe = client.subscribeStatus((status) => {
  console.log(status.state, status.path, status.connectionErrorCode);
});

// When the session ends:
await unsubscribe();
await client.close();
```

`connected` means the Locho host identity is reachable. It does not prove service
authorization, upstream availability or application login. Always validate your API
with `/health` and your own authenticated endpoint.

Use a session owner and an `AbortController` when switching tickets or leaving a
page. Close the client on explicit disconnect and do not silently replay interrupted
writes. The application owns credential persistence; browser storage is accessible
to scripts running on the website origin.

## Streaming Responses

Use `fetchStream` for SSE or another long-lived response. The response resolves after
headers, and the application owns parsing and reconnect policy:

```ts
const response = await client.fetchStream('/api/events', {
  headers: { Authorization: `Bearer ${sessionToken}` },
  setupTimeoutMs: 15_000,
  idleTimeoutMs: 75_000,
});

if (!response.ok) {
  await response.body?.cancel();
  throw new Error(`Stream failed: ${response.status}`);
}

const reader = response.body!.getReader();
try {
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    // Feed byte chunks to your bounded SSE/parser implementation.
    consumeEventBytes(value);
  }
} finally {
  await reader.cancel().catch(() => {});
  reader.releaseLock();
}
```

Always consume or cancel a streaming body. There is no automatic reconnection,
message replay or cumulative streaming body limit. Consumer pauses do not count as
upstream idle time, but an idle upstream read is bounded.

## Troubleshooting

### `invalidTicket`

Generate a fresh value with `locho share ... --ticket`. Use the complete `locho1:`
text and do not edit its contents. The parser rejects surrounding whitespace; an app
may trim clipboard input before parsing.

### `unsupportedService`

The initial browser SDK attaches TCP services. Generate the ticket from a service
with `type = "tcp"`, not an HTTP-service ticket.

### `startupTimeout`

Check that the host is running, the private API is reachable from the host, and the
browser can reach the Locho discovery/relay endpoints over HTTPS/WSS. A peer status
alone does not validate the service.

### WASM or CSP errors

Confirm that the `.wasm` response has `application/wasm`, the site is served over
HTTPS, and CSP `script-src` permits WASM compilation. For Vite, try the explicit
`initializeLocho(wasmUrl)` example above.

## API And Advanced Documentation

- [TypeScript API reference](https://www.npmjs.com/package/locho_web?activeTab=code)
- [HTTP request and response limits](https://github.com/trchopan/locho/blob/main/docs/browser-http.md)
- [Portable ticket format](https://github.com/trchopan/locho/blob/main/docs/tickets.md)
- [Runnable browser notes app](https://github.com/trchopan/locho_sdk_examples/tree/main/apps/web)
- [Browser SDK source and contributor checks](https://github.com/trchopan/locho/tree/main/packages/locho-web)

## Beta Limitations

- Browser-only; no native mobile bindings or server-side rendering.
- No browser-managed WebSocket API, cookie jar, redirect following or application
  authentication manager.
- Custom authenticated browser relays, real mobile-browser coverage, live relay
  migration and BFCache behavior remain separate acceptance work.
- The API transports bytes over an authorized TCP service. It does not expose a
  public URL or bypass your backend's authentication.
