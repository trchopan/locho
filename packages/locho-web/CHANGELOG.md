# Changelog

## 0.1.0-dev.2

- Add watch-backed status subscriptions with independent cancellation, a 16-observer
  cap, bounded delivery and retained terminal errors. Snapshot metadata now includes
  nullable connectionErrorCode/errorCode. No wire or ticket format changes.

## 0.1.0-dev.1

- Initial packaged TCP tickets, buffered HTTP, lifecycle and asset-loading proof.
