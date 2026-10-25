// Strict public-JS-path A/B diagnostic, NOT a publication/release gate.
import { spawn } from 'node:child_process';
import { mkdtemp, chmod, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const pkg = fileURLToPath(new URL('../', import.meta.url));
if (!process.env.LOCHO_HOST_BINARY) throw new Error('LOCHO_HOST_BINARY is required');
const directory = await mkdtemp(join(process.env.TMPDIR ?? tmpdir(), 'locho-policy-comparison-'));
await chmod(directory, 0o700);
const report = { schema: 1, releaseGate: false, publicJsConnectPath: true, maxCasesPerConfiguration: 30,
  startedAt: new Date().toISOString(), configurations: [] };
for (const canonical of [false, true]) {
  console.log(`Public-connect policy control: ${canonical ? 'canonical' : 'baseline'} (maximum 30 cases)`);
  const { output, status } = await new Promise((resolveResult, reject) => {
    const child = spawn(process.execPath, [resolve(pkg, 'scripts/reproduce-service-setup.mjs'), '--mode', 'n0', '--browser', 'chromium',
      '--scenario', 'stale_delayed', '--cycles', '30', '--canonical-n0-home', canonical ? '1' : '0',
      '--trace', 'none', '--host-trace', 'milestones', '--host-snapshots', '1'],
    { cwd: pkg, env: process.env, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '';
    child.stdout.on('data', (bytes) => { output += bytes.toString(); process.stdout.write(bytes); });
    child.stderr.resume();
    child.once('error', reject);
    child.once('exit', (status, signal) => resolveResult({ output, status: signal ? null : status }));
  });
  const evidence = output.match(/private evidence: ([^\n]+)/)?.[1];
  if (!evidence) { report.configurations.push({ canonical, status, harnessFailure: true }); break; }
  const summary = JSON.parse(await readFile(join(evidence, 'summary.json'), 'utf8'));
  report.configurations.push({ canonical, status, evidence, summary });
  await writeFile(join(directory, 'comparison.json'), JSON.stringify(report, null, 2), { mode: 0o600 });
  if (summary.harnessFailure || summary.inputsStable !== true || summary.credentialScan.rejectedFiles > 0
    || summary.traceCoverage.truncated || summary.traceCoverage.missingSuccessfulHostReplies !== 0) break;
}
const [baseline, canonical] = report.configurations.map((configuration) => configuration.summary);
report.sameHostAndWasm = Boolean(baseline && canonical
  && baseline.frozenInputs.hostSha256 === canonical.frozenInputs.hostSha256
  && JSON.stringify(baseline.frozenInputs.generated) === JSON.stringify(canonical.frozenInputs.generated)
  && JSON.stringify(baseline.frozenInputs.browser) === JSON.stringify(canonical.frozenInputs.browser)
  && JSON.stringify(baseline.frozenInputs.runner) === JSON.stringify(canonical.frozenInputs.runner));
report.baselineCapturedMatchingFailure = Boolean(baseline?.results.some((result) => !result.passed && result.tcpConnections === 2
  && result.clients?.some((client) => client.stage === 'health' && client.errorCode === 'timeout'
    && client.transportStats?.streamFramesRx === 0
    && client.hostTransportStats?.some((stats) => stats.streamFramesTx >= 1))));
report.canonicalCompleted = Boolean(canonical?.healthy && canonical.results.length === 30);
report.capturesUsable = report.configurations.length === 2 && report.configurations.every(({ summary }) => summary
  && !summary.harnessFailure && summary.inputsStable && summary.credentialScan.rejectedFiles === 0
  && !summary.traceCoverage.truncated && summary.traceCoverage.missingSuccessfulHostReplies === 0);
report.samePeerRelayAndBrowser = Boolean(baseline && canonical && baseline.peerRelayTag === canonical.peerRelayTag
  && baseline.results[0]?.browserVersion === canonical.results[0]?.browserVersion);
report.validContrast = report.capturesUsable && report.sameHostAndWasm && report.samePeerRelayAndBrowser
  && report.baselineCapturedMatchingFailure && report.canonicalCompleted;
report.finishedAt = new Date().toISOString();
await writeFile(join(directory, 'comparison.json'), JSON.stringify(report, null, 2), { mode: 0o600 });
console.log(`Strict policy comparison: ${report.validContrast ? 'matching baseline failure / canonical pass' : 'inconclusive or failed'}; private evidence: ${directory}`);
if (!report.validContrast) process.exitCode = 1;
