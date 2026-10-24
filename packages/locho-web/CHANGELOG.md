# Changelog

## Unreleased

- Add opt-in 15-minute metadata session soak tests (up to 60 minutes), covering
  pages/module service workers, slot/socket recovery and credential-free metrics;
  run nightly/manually, separate from short PR smoke checks.
- Add opt-in `fetchWithMetadata` and `fetchStreamWithMetadata` APIs returning a
  normal Response and immutable upstream header pairs, including repeated
  Set-Cookie fields, for application-managed sessions (#120).
- Preserve existing response APIs and streaming ownership; document credential
  handling and the distinction from browser-managed cookies.

## 0.1.0-beta.2

- Rewrite the README as a browser-app quickstart with ticket setup, first request,
  Vite/WASM deployment, status, streaming and troubleshooting guidance.
- Clarify the supported HTTP subset, application-owned authentication/storage and
  browser beta limitations.

## 0.1.0-beta.1

- First npm beta of the browser SDK, including ESM, TypeScript declarations and WASM.
- Portable TCP tickets, buffered and incremental streaming HTTP, bounded status
  subscriptions, operation cancellation and explicit client shutdown.
- No install-time native builds. Browser-only; WebSockets and custom relay
  authentication are not supported.

## 0.1.0-dev.2

- Add watch-backed status subscriptions with independent cancellation, a 16-observer
  cap, bounded delivery and retained terminal errors. Snapshot metadata now includes
  nullable connectionErrorCode/errorCode. No wire or ticket format changes.

## 0.1.0-dev.1

- Initial packaged TCP tickets, buffered HTTP, lifecycle and asset-loading proof.
