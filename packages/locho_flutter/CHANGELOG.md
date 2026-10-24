# Changelog

## 0.1.0-beta.3

- Refresh bundled Locho core/native sources with shared-peer stream lifecycle,
  failure-isolation and cancellation hardening, and iroh 1.4.0 transport support.
- Update ctutils to 0.4.4 to address RUSTSEC-2026-0385.
- Keep the public Flutter API, FRB runtime/codegen pin (2.12.0), wire protocol 3
  and ticket format 1 unchanged. Native sources are still bundled and built with
  Rust during consumer builds; no prebuilt native downloads are introduced.
- Foreground-only lifecycle and existing physical-device/network verification
  limitations remain. Browser upstream-header APIs are not Flutter APIs.

## 0.1.0-beta.2

- Rewrite the README as an app-developer quickstart with install, ticket setup,
  first request, Android/iOS configuration, lifecycle guidance and troubleshooting.
- Clarify that the SDK provides a foreground local tunnel while applications use
  ordinary Dart HTTP clients and own authentication/storage.

## 0.1.0-beta.1

- First pub.dev beta, with generated Dart bindings and Android/iOS integration.
- Bundle matching Locho core/native Rust sources in the distribution instead of
  fetching a separate Git revision during consumer builds.
- Foreground TCP attachments, portable tickets, bounded status observation and
  explicit cleanup. Rust/native build toolchains remain required.
- Android debug/release and 16 KiB emulator evidence; physical iOS runtime and
  broad physical-device/network acceptance remain open.

## 0.1.0-dev.2

- Bound cancellation of unchanged status subscriptions without closing attachments.
- Add observer capacity, bounded pause/resume delivery and retained terminal status.

## 0.1.0-dev.1

- Android-first foreground native attachment feasibility package.
- Persistent bridge runtime, typed Dart status/errors, loopback HTTP helpers.
- Android DNS/JNI and platform TLS initialization; debug/release device smoke.
- iOS build workflow added; physical iOS and external distribution remain pending.
