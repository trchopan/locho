# Browser first-service timeout investigation

## Release status

SDK beta.3 publication is blocked. Main's release preparation commit is
`c0334940194a01020cfdaaa410280e27f7bbe125`. A green ordinary CI run does not
establish that intermittent public-N0 first-service timeouts are resolved.

No experimental mitigation below is merged or approved for publication. The
observed failures occur in the health proof without using the metadata APIs.

## Observed failure boundary

Controlled tests use disposable CLI state, synthetic capabilities, two independent
browser clients, and a local HTTP backend reached through the real N0 relay.

- A failing client completes peer connection and reports `connected/relay`.
- Backend HTTP request counts are `[1, 0]` instead of `[1, 1]`.
- A more detailed baseline reproduction counted **two upstream TCP connections**
  but only one HTTP request. The failed call timed out inside `session.open_tcp`,
  before the HTTP write. Thus the failed client's service authorization request
  reached the host sufficiently far to open its upstream TCP connection, but
  service setup did not complete at the browser.
- Temporary local instrumentation also observed that the failing browser endpoint
  was **not registered/online at connect return**. That is a correlation, not proof
  that it remained offline at timeout or that missing registration is the cause.
- No finer failing QUIC/authorization sub-stage or definitive upstream defect has
  been established. An HTTP body parser/EOF-only explanation does not explain the
  reproduced pre-HTTP failure.

All diagnostics use fixed stages and aggregate counters. Temporary diagnostic
error codes and fields are not part of the public SDK and are removed from the
current production source. Do not upload capabilities, cookie/header values or
unreviewed transport traces.

## Results retained, including unsuccessful experiments

| Run | Result | Interpretation |
| --- | --- | --- |
| Baseline concurrent stale hints, 10 repeats/engine/delay | 59/60 passed; one pre-HTTP timeout | Reproduction with backend HTTP counts |
| Baseline with temporary stage codes | 60/60 passed | Did not capture a finer failure stage |
| Pre-resolve cached hint before dialing | 238/240 passed; stale-hint and valid dotted-relay timeouts | Not a demonstrated fix; PR #123 closed without merging |
| Baseline stage investigation, original 30-second test limit | 6 passed, then overall test timeout | Lost diagnostics because existing startup/request/cleanup phase budgets can exceed the reporting window |
| Baseline with 60-second diagnostic reporting window | 2 passed, then service-setup timeout; TCP connections 2, HTTP counts `[1, 0]` | SDK budgets unchanged; wider window allowed the existing timeout to be reported |
| Baseline registration-state investigation | 8 passed, then service-setup timeout with registration incomplete at connect return | Correlation only |
| Wait for relay registration before dialing, within existing startup budget | 160 passed, one WebKit explicit-relay startup timeout; 79 did not run | Not a clean release gate; does not establish whether registration or the remaining peer budget expired |

The reporting window was restored to the original 30 seconds after diagnosis.
Application requests were not replayed, assertions were not disabled and SDK
timeouts were not extended. The registration-barrier experiment is retained only
in local investigation commit `0f65f5d`, not in the release branch. Its observation
does not justify silently changing release acceptance or claiming a fix.

## Reproduce and collect safe evidence

After a fresh WASM/production build, use the existing N0 fixture environment:

```sh
CI=1 LOCHO_N0_INTEGRATION=1 \
LOCHO_HOST_BINARY=/absolute/path/to/locho \
LOCHO_N0_FIXTURE_FILE=/private/path/to/disposable-fixture.json \
npx playwright test tests/n0-integration.spec.ts \
  --grep 'concurrent stale-hint' --repeat-each=30 --max-failures=1
```

The failure comparison now includes per-authority HTTP counts. The attachment
`upstream-first-request-counts` also records the aggregate number of upstream TCP
connections opened during the case. Aggregate TCP counts can include other tests
in parallel local runs, so they are observations rather than per-client assertions.
Use one worker for isolation. The ordinary success requirement remains exactly one
HTTP request per client and the expected successful first response.

## Next investigation gates

1. Reproduce against a controlled relay to separate public relay/network conditions
   from local SDK/QUIC behavior, while retaining public-N0 coverage and its failures.
2. Correlate host authorization-response enqueue/delivery and browser handshake
   reads with reviewed, disposable transport traces; do not infer an upstream cause
   from `connected` status alone.
3. Require a targeted regression and full source/package/soak validation for any
   further mitigation. Do not retry unchanged CI until it happens to be green and
   treat that as proof of resolution.
4. Publishing despite this pre-existing beta limitation requires explicit maintainer
   acceptance and a documented exception; this investigation does not grant it.
