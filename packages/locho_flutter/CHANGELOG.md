# Changelog

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
