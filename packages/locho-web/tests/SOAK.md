# Metadata smoke and soak checks

The regular browser suite includes short metadata smoke/regression coverage across
Chromium, Firefox and WebKit, including classic/module service workers. Run just
that coverage with `npm run test:smoke` (requires the N0 fixture variables below).
Without `LOCHO_N0_INTEGRATION=1`, transport-dependent smoke tests are skipped; a
page-load-only success does not validate metadata.

The opt-in soak runs the same sustained workload in both a page and a **module
service worker** on all three engines (six parallel tests). Default duration is
15 minutes **per test**, configurable from 1 to 3600 seconds. It is excluded from
ordinary `test:browser`, `test:smoke` and `test:external` runs.

## Local execution

Build the host from the repository root with `cargo build --locked --bin locho`.
Then, from `packages/locho-web`:

```sh
npm ci
npm run build:wasm
npm run build:production
npx playwright install chromium firefox webkit

export LOCHO_N0_INTEGRATION=1
export LOCHO_HOST_BINARY="$PWD/../../target/debug/locho"
export LOCHO_N0_FIXTURE_FILE="${TMPDIR}/locho-metadata-soak-fixture.json"

npm run test:smoke
LOCHO_SOAK_SECONDS=900 npm run test:soak
```

The global fixture starts a disposable upstream and a real Locho host using the
official N0 relay/discovery network. Network connectivity is required. Do not run
separate invocations concurrently with the same fixture path or default port 4173.
The packaged external consumer is checked separately by `npm run test:external`;
the soak uses the production browser bundle, not an installed npm package.

For a quick harness check use `LOCHO_SOAK_SECONDS=20`; that is **not** endurance
evidence. For one engine, append `-- --project=chromium`. Longer runs may finish
slightly beyond the requested duration to complete the last bounded batch and
cleanup. The test timeout includes three additional minutes for setup/cleanup.

## Workload and gates

- Reuse a client across repeated login → explicitly authenticated request → logout
  → unauthenticated request cycles, alternating buffered/streaming metadata APIs.
- Verify separate repeated cookies and expiry/deletion attributes, immutable pairs,
  and expected 200/204/401 statuses; session values stay inside the browser context.
- Alternate pending-read cancellation and `AbortSignal` cancellation.
- Every five cycles, occupy all 16 HTTP operation slots, verify `resourceLimit`,
  cancel all bodies and confirm a subsequent request succeeds.
- Every ten cycles, close the client with unread and pending-read streams, verify
  both fail with `closed`, then reconnect using the same ticket.
- After each five-cycle batch and after final cleanup, require the fixture's
  **actual upstream socket count for that test's authority** to return to zero.
  This is independent of the fixture's single-stream `active` flag.
- Fail immediately on any invariant or transport failure. Do not retry or replay
  failed writes silently. These checks validate resource recovery, not automatic
  application recovery from host restarts or browser worker eviction.

## Diagnostics and limitations

`test-results/soak-results.json` contains the Playwright result summary. Each test
attaches `metadata-soak.json` with completion/cleanup flags, total cycles, reconnects,
capacity checks, cycle latency p50/p95 upper bounds and maximum, and bounded samples.
Only fixed assertion codes and allowlisted aggregate data are reported. Traces,
screenshots and videos are disabled; do not enable them with real credentials.

Samples include fixture-process RSS and, where available, page JS heap usage.
Fixture RSS is shared across tests, **not host/browser RSS**. Service-worker heap
measurements are unavailable and reported as such. At most 360 samples are retained,
preserving the first post-warmup sample and the most recent samples. Memory values
are observations, **not a cross-browser leak assertion**: GC and WASM memory growth
make a fixed heap-slope threshold unreliable. Socket/permit recovery are hard gates;
latency and memory trends are for investigation, without arbitrary pass thresholds.

## CI

`.github/workflows/browser.yml` keeps normal regressions/smoke on PRs and main pushes.
A separate soak job runs nightly at 03:30 UTC and on manual dispatch, using the
freshly built production browser bundle and host. The manual `soak_seconds` input
defaults to 900 and supports up to 3600. The job uploads credential-free aggregate
diagnostics even on failure; the temporary fixture containing the ticket is never
uploaded. Soak requires the preceding build/regression job to succeed.
