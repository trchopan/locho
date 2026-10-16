# SDK status observation

Observation is independent of client/attachment ownership. Cancelling an observer
does not close the owner or its traffic. Snapshots contain increasing revisions;
updates may coalesce. `connected` authenticates the host identity, not service/API
readiness. Each owner permits at most 16 active wrapper observers. Excess observers
fail with `resourceLimit`; released observers return capacity.

## Flutter

```dart
final subscription = attachment.statuses.listen(updateStatus);
await subscription.cancel(); // attachment remains usable
```

The first event is the current snapshot, including retained terminal status for
closed attachments. Then each subscriber has at most one native `next_status` wait
in flight. Its existing one-second native timeout bounds ordinary cancellation
latency (bridge/runtime scheduling adds overhead). Cancellation stops scheduling
new waits, suppresses late events/errors and waits for that one operation to settle.
There is no need to close the attachment merely to unsubscribe.

Pause holds at most the current in-flight update and stops further native waits.
Resume continues with coalesced revisions; no queue of native updates accumulates.
Terminal closed status ends the stream. Final snapshot and error categories remain
available on the attachment. OS suspension can delay timers/execution; no real-time
deadline is guaranteed while the process is suspended. The wrapper limit applies
to public Dart observers, not direct generated/internal bridge use.

## Browser

```ts
const unsubscribe = client.subscribeStatus(async status => {
  updateConnection(status);
}, { signal, onError: error => showStatusError(error.code) });
await unsubscribe();
```

`subscribeStatus` returns an idempotent async unsubscribe function. First delivery
is asynchronous and reads the current Rust watch snapshot. The next revision waits
on the watch receiver, not a polling timer. One pending watch wait per observer and
serialized listener calls prevent unbounded delivery queues. Slow listeners coalesce
revisions. Unsubscribe/AbortSignal wakes the pending wait and frees the observer
permit; an already-running application callback cannot be forcibly cancelled, but
unsubscribe does not wait for its arbitrary promise. No new callback starts afterward.

Listener rejection/throw terminates only that observer and invokes optional onError
with a sanitized error. An onError throw is contained. Already-aborted registration
is a no-op. Client close wakes slow listeners so observers can read retained terminal
status and finish; late observers receive the closed snapshot and finish. Callback
code already running may still finish after client close or unsubscribe.

Snapshots and subscriptions share `revision`, `state`, `path`, nullable
`connectionErrorCode` (latest peer error) and `errorCode` (retained terminal cause).
No detailed transport addresses, credentials or raw exception text are included.
Limits are acquired when the observer is created and held until its pump finishes.

## Evidence and rollout

Deterministic Flutter tests cover unchanged-wait cancellation, pause/resume,
terminal completion and capacity. Android emulator integration cancels while
connected, then issues a successful health request before closing the owner.
Production browser and external-tarball tests cover independent cancellation,
slow/throwing listeners, AbortSignal, capacity, increasing revisions, close and
late terminal observation across Chromium/Firefox/WebKit.

Application upgrades and new prerelease artifact pins remain a separate PR. Existing
examples still consume their original checksum-pinned artifacts until deliberately
updated. Physical network transitions and device suspension acceptance remain open.
