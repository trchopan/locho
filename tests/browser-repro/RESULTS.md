# Initial controlled reproduction evidence — 2026-10-24

Investigation evidence only. This is **not a fix, clean release gate or registry
acceptance**. Production startup behavior, service authorization and request
deadlines are unchanged. Source includes uncommitted tracing/harness work based on
`8cd7a1e`; summaries explicitly record `sourceDirty:true` and dependency versions.

| Configuration | Outcome | Scope |
| --- | --- | --- |
| Local relay, milestone tracing, 3 repetitions of each scenario on Chromium/Firefox/WebKit | 27 cases / 54 successful clients | Explicit/stale/delayed lookup controls; both reply writes and client decodes observed |
| Local relay, forced 20-second pre-reply delay, Chromium | 3 expected-fault cases / 6 authorization timeouts | Two upstream TCP sockets, zero HTTP requests; host never reaches reply-write milestone. Deliberately unhealthy, not a naturally reproduced bug |
| Public N0, instrumented browser transport trace, Chromium | 30 cases passed | No natural failure captured; the initial launcher missed CLI stdout, so host trace coverage was incomplete. Not a complete two-sided trace |
| Local relay, no tracing, Chromium | 30 cases / 60 successful clients | Serial untraced control, no observed failure |
| Public N0, **unchanged verified beta.3 tarball**, host milestones only, Chromium | First 11 cases passed; case 12 failed | Natural timeout reproduced with original browser artifact; no browser trace initializer or changed client error codes |
| Local split relays, 250ms browser-home TCP-chunk delay, Chromium, 3 repetitions | 9 cases / 18 successful clients | Two-sided milestone capture complete; no natural stall; no trace truncation or known-credential matches |
| Public N0, instrumented debug transport trace, Chromium, 30 repetitions | 90 cases / 180 successful clients | Complete two-sided milestones; no natural failure; this predates the QUIC frame-level trace build |

The exact package baseline integrity is
`sha512-pUV0NDpn7A2YsFrTi/Rkc/EeUDGXwIaNSc1z3Wwv1kDlJwGJlh4xipDmRGDskEvPt4DTVEJJbdphQCNVShbNBA==`.
Its `build.json` reports clean source
`d930c62f57459aae8f87a62b103984de4785412e`, version `0.1.0-beta.3`, wire protocol 3,
ticket format 1. It is the retained candidate tarball, not a published registry
version or the final merged release SHA.

## Naturally failing package case

- Chromium 156.0.8078.4, delayed stale hint (750ms), repetition 4.
- Client 0: connected at approximately 3344ms, successful health at 3580ms, closed
  at 3668ms from its start.
- Client 1: connected at 3515ms; health timed out at 18519ms (about 15004ms later),
  still reporting peer `connected/relay`; cleanup finished at 22559ms.
- Two upstream TCP connections opened. HTTP request counts were **`[1, 0]`**.
  All sockets were reclaimed after cleanup.
- In this specific case's host trace range, **both authorization replies completed
  their stream write/flush calls**. This means bytes were accepted/queued by the
  host transport API, **not proof of QUIC delivery or browser receipt**.
- This differs from the injected control, where the reply-write milestones are
  absent. It narrows the unresolved boundary to host transport delivery versus
  the browser authorization receive path; it does not identify the exact upstream
  defect or prove missing relay registration is causal.

Known-credential scans rejected zero files and no trace bytes were dropped in these
runs. Scans are a safety net, not a guarantee of complete redaction. Raw evidence
remains in mode-0600 files under private temporary run directories; keys/state were
deleted. Only allowlisted counters/timings are recorded here, not credentials,
packet contents, endpoints or raw traces.

Initial healthy/fault smoke jobs overlapped to validate isolation. These are not
paired causality results. The untraced control and exact-package baseline were
subsequent serial runs. The split-relay delayed control also ran serially. Complete
two-sided capture of a naturally failing transport remains further investigation.

Harness validation: 44 core tests, 16 browser Rust tests (including the private
feature), 77 CLI tests and 5 Node runner/publication-guard tests passed. These are
correctness checks for the instrumentation/harness, not proof the network defect
is fixed.

## Completed debug-only capture

The 90-case run's passive connect-return snapshots reported `online:false` for
**107 successful clients** and `online:true` for 73. This does not rule out a
registration race, but shows that an incomplete snapshot at connect return is not
sufficient to cause the timeout. It is not a basis for reinstating an unconditional
registration barrier. Two-sided milestone totals were complete (900 host, 1080
browser); no bytes were truncated and no known credentials were matched.

A subsequent fixed comparison matrix uses one frozen instrumented WASM build and
one host binary, with 30 cases maximum per configuration, serial execution and
first-failure stopping: unchanged package/host milestones, instrumented browser
tracing disabled/host milestones, two-sided milestones, and two-sided QUIC frame
tracing. These are distinct planned experiments, not retries of failed release
validation. Run summaries verify file hashes before/after execution and retain
source/build/lock/browser provenance.

## Frozen four-configuration comparison

All configurations used the same host binary and Chromium 156.0.8078.4. The three
instrumented configurations used exactly the same WASM/bundle. Each configuration
had a fixed 30-case limit and stopped at its first failure. All before/after input
hashes were stable; known-credential scans rejected zero files; captures were not
truncated.

Frozen SHA-256 identifiers (dirty investigation inputs, not release provenance):

- Host binary: `0e6efed745cdfd8cd7524ae257833d03574ba4b71208fe2cd3ecf1a685b967f2`
- Instrumented WASM: `c4134e63f9d4bfa982d45e83d97acab46feddcf27fb2eae678c89724a373e255`
- Workspace lock: `1f10c4dc2b67fa5a1e0f6bef2a6b4a0c55a701c0943b20ac666ccb20a350798a`

iroh/iroh-base/iroh-relay and noq/noq-proto/noq-udp remained at 1.4.0; Tokio 1.53.3,
rustls 0.23.46 and ctutils 0.4.4. Full source/generated/bundle hashes are retained in
the private comparison summaries.

| Configuration | Result |
| --- | --- |
| Unchanged tarball, host milestones | 30/30 cases passed |
| Instrumented SDK, browser tracing disabled, host milestones | 8 cases passed; case 9 failed, delayed stale hint |
| Same SDK, two-sided milestones | 17 cases passed; case 18 failed, delayed stale hint |
| Same SDK, two-sided QUIC frame tracing | 30/30 cases passed |

Both failures had two upstream TCP connections, HTTP counts `[1, 0]`, and complete
socket cleanup. Client 1 failed after its unchanged approximately 15-second health
budget, still reporting `connected/relay`. Both host authorization reply writes
completed. In the two-sided milestone failure, both client authorization requests
were written, but **only one browser completed the four-byte reply prefix read**
and decoded authorization. The failing client reported `online:false` at connect
return; that remains correlation, not a cause.

Frame-level capture coverage was checked independently of milestone totals: it
contained 180 host STREAM frame sends and 180 browser STREAM frame receives across
60 successful clients, plus ACK/loss metadata. No natural failure occurred there.
The matrix does not establish that tracing prevents the bug: the unchanged package
also passed this sample, while it failed in the earlier exact-package run.

Next bounded capture narrows the observer: **host QUIC frame tracing + browser
milestones**, same frozen inputs, maximum 30 cases and first-failure stopping. This
tests the missing transmission/acknowledgment boundary without enabling verbose
browser transport logs. No production mitigation has been applied.

That host-transport/browser-milestones capture also passed 30/30 cases, without
truncation. A further narrowly filtered capture enables only the host packet-builder
trace target (outbound frames) plus milestones and connection-driver spans. This
reduces trace volume substantially; it can establish frame queuing/transmission
attempts but **cannot establish peer acknowledgment**, since inbound ACK tracing
is intentionally absent. It retains the same 30-case bound and first-failure stop.

The low-volume outbound-frame capture also completed 30/30 cases successfully,
with stable input hashes and no truncation or known-credential matches. These
additional passes still do not establish an observer effect or a fix. Investigation
now returns to the local split-relay control with a fixed 1000ms browser-home
TCP-chunk delay (rather than 250ms), up to 30 cases, to test whether a slower
registration transition can reproduce the authorization boundary locally. The
unchanged SDK startup/request budgets remain in force; a connect timeout would be
a different signature, not counted as reproducing the service-authorization stall.

The 1000ms split-relay control passed all 30 cases with complete milestone capture,
stable inputs and no known-credential matches/truncation. This weakens a simple
"registration delay alone" explanation, but does not exclude a specific path or
registration transition defect.

The next diagnostic changes the observer rather than the case budget: the isolated
`n0-trace` host reuses production host and N0 endpoint-builder source, buffering
transport records in memory and dumping only after each case. It is not the exact
CLI artifact and its standalone feature graph is a separate experimental variable.
The 30-case/first-failure bound and browser milestones remain unchanged. Two buffer
unit tests verify drain behavior and explicit truncation/eviction accounting.

The buffered native capture completed 30/30 healthy cases with stable inputs, 31
successful dumps and zero buffer/trace truncation or known-credential matches.
Coverage review found the new binary's Rust target is `n0_trace`, not the original
fixture's `locho_browser_repro`; the filter therefore omitted host reply milestones
(120 core protocol milestones remained). QUIC frame capture was enabled, but this
must **not** be described as complete two-sided milestone evidence. Correcting the
filter is a harness repair, not a transport fix.

Next: a new private WASM build exposes allowlisted aggregate connection counters,
collected **only after an operation has already failed**, before close. Successful
operations do not inspect counters, and browser live tracing is disabled. The
30-case bound is unchanged. Zero received STREAM frames would rule out an
application read-wake explanation for that capture; a nonzero frame count alone
would not prove complete authorization-byte delivery (frames can be partial or
zero-length). This diagnostic feature/export is excluded from normal SDK builds
and covered by the publication guard.

## Natural failure with a post-timeout browser counter snapshot

With browser live tracing disabled and the original CLI host logging milestones,
29 cases passed and case 30 failed (delayed stale hint, client 1). All input hashes
were stable; both host authorization reply writes completed; HTTP counts were
`[1, 0]`, two upstream sockets opened and all sockets were reclaimed. No capture
truncation or known-credential matches occurred.

Counters sampled after the failed health operation, before client close:

| Browser transport counter | Value |
| --- | --- |
| Received STREAM frames | **0** |
| Transmitted STREAM frames | 2 |
| Received datagrams / bytes | 2 / 1338 |
| Transmitted datagrams / bytes | 15 / 10873 |
| Received ACK / PATH_ACK frames | 1 / 0 |
| Transmitted / received PATH_ABANDON frames | 0 / 0 |
| Packets declared lost | 3 |

This rules out an application read-wake failure as the primary cause **for this
capture**: no peer STREAM frame reached the browser QUIC connection. It does not
prove the host transmitted the reply on the wire, establish which relay dropped
traffic, or show that the three lost packets carried authorization bytes. Reset
and STOP_SENDING counters include cancellation after timeout and must not be
mistaken for pre-timeout causes.

Next bounded capture uses the disposable host's passive connection registry and
post-failure signal snapshot. The browser requests that host snapshot only after
failure, before close removes the failed connection. Both sides log milestones
only (browser live tracing remains disabled), avoiding per-frame logging during
the request. The registry is gated behind `integration-test` and explicitly enabled
only by the disposable binary; ordinary/release builds do not activate it. This is
still diagnostic work, not a production repair.

That paired-counter attempt with the disposable host passed 30/30 cases. Corrected
filter coverage included all 60 host reply milestones; input hashes were stable,
all 31 dumps completed, and there was no truncation or known-credential match.
It did not produce paired failure counters.

To remove the standalone-host feature/logging-wrapper variable, the real CLI now
has an **integration-test-only**, explicit-environment snapshot hook. Its ordinary
logging and host execution remain unchanged; it does no per-packet inspection.
The hook is absent from ordinary builds and inert in integration builds unless
the fixture provides its private snapshot path. The next 30-case run uses this CLI
observer, browser tracing disabled, and post-failure snapshots before close.

The original frozen CLI artifact remains retained as
`target/browser-repro/debug/locho-baseline-0e6efed7` with its previously recorded
SHA-256. The test-only snapshot CLI has SHA-256
`1eac8b2b0e7ff73df5e69bf7b684646a81f0badfdb6982d86758bc3f55c31c3d`;
it must not be presented as the byte-identical baseline or a release candidate.

The real test-only CLI snapshot run passed all 30 mixed-scenario cases, so no
paired failure snapshot was obtained. Because every natural failure captured by
this harness so far occurred in `stale_delayed`, the next bounded experiment
allocates the same 30-case total to that scenario only (rather than ten cases of
each of three scenarios). It keeps the same CLI/WASM, deadlines, exactly-once
assertions and first-failure stopping. This is a targeted diagnostic refinement,
not a larger release gate, replay of failed cases, or evidence the defect is fixed.

## Paired natural failure: reply frame constructed, no browser stream frames

The focused CLI-snapshot run failed on case 12 after 11 passes. This time client 0
failed while client 1 succeeded (HTTP counts `[0, 1]`), so this is not specific to
the second client index. Both host authorization replies were queued. The failed
browser received 0 STREAM frames / 2 datagrams; the still-open host counterpart
reported 2 received STREAM frames, **1 transmitted STREAM frame**, 23 transmitted
datagrams, and **0 packets declared lost** at the post-failure snapshot. All socket
resources were eventually reclaimed; input hashes and capture coverage were valid.

This places the reply beyond the application write boundary and into QUIC frame
construction. It is not proof of wire delivery, acknowledgment, or retransmission
correctness. STREAM-frame transmit counters count construction attempts, not peer
consumption. No PATH_ABANDON frames were reported by either side.

A subsequent new diagnostic build added post-failure path counters and failed
again on case 12. Both sides had exactly one selected relay path (ID 0), with the
same normalized relay tag. Host path: 1 STREAM frame transmitted, 0 lost packets,
21 transmitted datagrams, RTT 133ms. Browser path: 0 STREAM frames received, 2
received datagrams, RTT 2316ms. The browser's close also hit its existing shutdown
timeout; backend sockets were still reclaimed. This weakens the specific
abandoned-path hypothesis; it does not establish why recovery stalls.

## Relay URL alias hypothesis (not yet a fix)

Source review found that production browser peer hints/discovery URLs remove DNS
root dots, while `presets::N0` still constructs its client home-relay policy from
dotted default URLs. Completed successful verbose captures contain relay actor
spans for both dotted and non-dotted spellings of the same relay within one
endpoint (40 such endpoint/relay groups in that 30-case capture). This confirms
duplicate alias handling exists, **not that it causes the failure**.

The local fixture now supports `localhost.` and `localhost` aliases of the same
physical relay, with an experimental private home-policy normalization toggle.
Unshaped aliases passed 10 cases; aliases with a 1s authorization delay passed 3;
delaying the first two browser relay HTTP upgrades by 1s also passed 3. Those
controls do not reproduce the defect and must not justify a production patch.
The next local control explicitly holds dotted-alias upgrades until both upstream
TCP connections open, then releases them before the delayed authorization reply.
This tests a specific registration ordering instead of further ambient N0 trials.

That held-upgrade control passed all 3 cases too. The first two cases released
dotted upgrades after two upstream TCP accepts; later close/fallback releases were
also observed, so not every attempt exercised the intended ordering. No alias
control has reproduced the natural authorization timeout. We therefore do not
claim this hypothesis is proven by the local controls.

An **investigation-only** N0 entry point now canonicalizes the client default relay
policy, preserving the same servers, QUIC discovery settings and auth configuration,
then uses the normal lookup/session/authorization path. It does not pre-resolve,
await registration, replay requests or change deadlines. The ordinary SDK connect
path is still unchanged. A unit test verifies production/staging map membership
matches normalized peer keys and preserves relay options. The private export is
rejected by the publication guard. A bounded 30-case focused run is testing this
candidate; passing it alone would not establish a fix.

The canonical-policy candidate passed 30/30 focused N0 cases (60 clients), with
stable inputs, all 60 host reply milestones present, and no truncation or
known-credential matches. Private WASM SHA-256:
`cff620c93b513d33ed50a8faa42d6832c256c4a5a979d86101a50bc5177211e8`;
test CLI SHA-256:
`f7e15b5d9515e6708264afb830a475eabaea5ae2d87495871079a3b49c7bf11d`.
The next control runs the ordinary baseline connect entry point from the exact
same WASM, against the same host with the same snapshots and focused 30-case
budget. This isolates the policy toggle from a new WASM build or verbose tracing.
Production behavior remains unchanged pending the matched control and larger
verification; no release exception is inferred from this diagnostic pass.

The matched ordinary baseline failed **case 1** with the same WASM/host hashes:
two upstream TCP sockets, HTTP `[1, 0]`, browser received 0 STREAM frames, host
constructed 1 STREAM frame, and neither side reported path abandonment. Captures
were complete and inputs stable. This strengthens the candidate but is not a
deterministic network reproduction or a completed release gate.

The candidate is now undergoing a predeclared maximum of 270 mixed cases across
Chromium/Firefox/WebKit (30 repetitions of each of 3 scenarios per browser),
stopping at the first failure. This expansion follows review of the matched
failure/pass evidence; it is not a retry of unchanged release CI.

Meanwhile, a separate clean-main-based worktree on `fix/browser-relay-policy`
contains only a proposed client-policy normalization and regression tests. The
policy-key test failed against the original configuration and passed after the
small repair; all 18 Rust browser tests passed. It statically proves the alias-key
mismatch and preserves server membership/options. The live candidate gate and
full ordinary source/package/soak validation remain pending.

### A/B orchestration caveat found during review

The private candidate constructor directly wraps a raw WASM connection, whereas
the ordinary baseline goes through `LochoClient.connect()` and its additional
cached initialization await/cancellation wrapper. Although host and WASM bytes
are identical, this is a possible scheduling confound. The observed failure/pass
contrast must not be described as isolating ONLY the relay policy. The ongoing
270-case run is useful candidate coverage but does not resolve that caveat.

Before promotion, the private harness will use a synchronous investigation-only
policy toggle before connecting, then invoke the unchanged public
`LochoClient.connect()` path in both variants. This preserves JS orchestration and
changes only endpoint relay-policy construction. The ordinary SDK build has no
toggle/export, and the separate minimal production proposal has no diagnostic
feature. A fresh paired control is required after that harness refinement.

The isolated minimal proposal is committed locally as
`da9060d05dc8a79b69ce92fbdb28cdeb5c4c150d` on `fix/browser-relay-policy` (two Rust
files only). Its policy-key regression was red before the repair; all 18 browser
Rust tests and strict Clippy passed afterward. Fresh ordinary WASM, TypeScript,
package and production builds passed, with no investigation exports. Package
`build.json` records that commit, `dirty:false`, wire protocol 3 and ticket format 1.
A separately built ordinary CLI reports the same commit and `clean`. These are
preparation checks for subsequent ordinary public-API/source/package/soak tests,
not completed live acceptance or final merged release provenance. Nothing has
been pushed, merged or published for this proposal.

## Stricter public-JS-path control: inconclusive, retained

The private raw candidate completed 270/270 mixed cases (540 clients) across the
three browsers. Coverage included all 540 host reply milestones, stable inputs,
and no truncation or known-credential matches. The orchestration caveat above
still applies to that run.

The harness was then refined to configure a synchronous private policy bit before
starting clients; both variants now execute the unchanged public
`LochoClient.connect()` path, including cached initialization and cancellation.
A Node test checks both variants invoke that same path. The obsolete private raw
N0 constructor was removed; all private toggle exports remain publication-blocked.

The predeclared strict comparison completed **30/30 baseline and 30/30 canonical
cases**. Host/WASM/bundle/runner hashes, browser version and peer relay tag matched;
captures were usable with full host milestone coverage. Because the baseline did
not reproduce the defect, `validContrast:false` and process exit 1 correctly mark
the result **inconclusive**, not a verified mitigation or a canonical failure.
This control will not be repeated until a favorable contrast appears.

Consequently, the configuration mismatch has a deterministic red/green unit
regression, but its causal role in the intermittent transport stall remains
unproven. Ordinary, non-instrumented production-source acceptance is now running
against the minimal proposal; installed-package and soak gates remain pending.
Those gates do not silently waive this causal uncertainty or authorize publication.

## Ordinary minimal-proposal acceptance

Fresh ordinary production WASM/JS (no diagnostic feature/exports) from commit
`da9060d` passed the full source browser suite: **96 passed, 3 expected
external-consumer-only skips**, across Chromium/Firefox/WebKit. The host was an
ordinary CLI built from the same commit with enforced clean provenance; the pinned
reference API was included. No deadlines or test assertions were changed.

Installed-tarball acceptance is running separately against a fresh npm consumer;
the exact package and 900-second soak have not yet been accepted. The full
repository `cargo deny check -W unmaintained` security/license/source gate passed
under the unchanged CI policy, with existing duplicate-version and unmaintained
`paste` warnings. The stricter default `cargo deny check advisories` invocation
reported that known unmaintained advisory as an error; no policy/configuration was
changed. `cargo audit` itself is unavailable locally and was not claimed to pass.

Exact packed SDK acceptance subsequently passed **99/99** in a freshly installed
consumer across the three engines. The retained local candidate archive is under
the `fix/browser-relay-policy` worktree, not the older preparation archive in the
original checkout:

- Source: clean `da9060d05dc8a79b69ce92fbdb28cdeb5c4c150d` (not final merged SHA).
- Tarball: `packages/locho-web/dist/artifacts/locho_web-0.1.0-beta.3.tgz`.
- SHA-256: `735c3392b6fbee4569de1bff75491844e69b681c572e3ee51537bd24a4556926`.
- Integrity: `sha512-ajMixaJ9pZ6s3AKeJpLanePnWlbnicneNJT9aceCuscI19Nnf1tDA3ohmsmvr4eYZw0T9AcijkEP1uVXj6Q6vw==`.
- Wire/ticket formats remain 3/1. No investigation exports are present.

The first full Rust CI invocation exceeded the shell tool's 120-second execution
budget after passing early tests; it was not a completed gate or an assertion
failure. The unchanged clean-checkout `scripts/verify-ci.sh` was then run without
that tool cutoff and passed: formatting, strict workspace Clippy, 178 workspace
tests plus 29 minimal-core tests; four expected release-artifact-only ignores.
No SDK/test deadline or assertion was altered for that rerun.

Draft repair PR: https://github.com/trchopan/locho/pull/124. It contains only the
two-file policy repair/regressions and explicitly records the causal uncertainty.
Hosted CI and a fresh ordinary six-context 900-second soak are now running. The
proposal has not been merged; no SDK publication or release exception is authorized
by these results.

The ordinary 900-second soak subsequently passed all **6 browser/context cases**,
with retries disabled and 3,555 total cycles. Chromium page/worker: 595/600;
Firefox: 595/590; WebKit: 590/585. Every case reported `completed:true` and
`cleanupSucceeded:true`; measured elapsed time was at least 900 seconds in each
case. The runtime was the ordinary production bundle from `da9060d`, not the
diagnostic/private candidate. This completes local candidate acceptance, not
final-merged-SHA release acceptance or proof of the intermittent timeout's cause.
Hosted CI and maintainer review remain pending; the PR stays draft and unpublished.

Hosted CI subsequently completed with all non-skipped checks successful on the
validated PR head `da9060d`: browser/package workflow `40113554557`, desktop/MSRV/
packaged artifact/security workflow `40113554584`, and Flutter Dart/codegen/Android/
iOS workflow `40113554554`. Scheduled metadata/native soaks and tag-only release
jobs were skipped by design on the PR; they are not claimed as executed. The
six-context 900-second evidence above is the local ordinary-bundle soak.

The PR remains open/draft for maintainer review, with its causal uncertainty
explicitly stated. The working tree is clean, the repair is only two Rust files,
and no background validation jobs remain. No merge, tag or SDK publication has
occurred. Publishing still requires review, final merged-commit acceptance and
registry-consumer checks; these candidate results do not create an exception.

## Upstream hypotheses to compare against captured evidence

Read-only upstream review found several unresolved reports; none is yet established
as the cause here, and no upstream patch has been applied:

- [iroh #4603](https://github.com/n0-computer/iroh/issues/4603): a full queue for one
  relay blocks sends through other relays. Its reproduction saturates an obsolete
  relay; our first-service failure has not yet shown that saturation.
- [iroh #4599](https://github.com/n0-computer/iroh/issues/4599): dropped transmits
  retain relay channel permits until their connection closes. Reported exhaustion
  involves many live connections, unlike the fresh two-client browser fixture;
  check evidence rather than assume equivalence.
- [noq #770](https://github.com/n0-computer/noq/pull/770): draft fix for delayed
  retransmission of in-flight stream data after path abandonment. Potentially
  relevant only if the failing trace shows a path transition with unrecovered
  authorization bytes. The proposal remains a draft, not a verified fix for Locho.
- [iroh #4600](https://github.com/n0-computer/iroh/issues/4600): a new connection is
  pinned to an existing connection's dead selected path after peer restart. The
  controlled fixture has fresh identities and no restarted host, so its stated
  trigger does not currently match.
