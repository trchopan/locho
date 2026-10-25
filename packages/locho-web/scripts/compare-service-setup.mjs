// Fixed experiment matrix, not retry-until-green release validation.
import { spawn } from 'node:child_process';
import { mkdtemp, chmod, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { sha256 } from './repro-provenance.mjs';

const pkg = fileURLToPath(new URL('../', import.meta.url));
if (!process.env.LOCHO_HOST_BINARY) throw new Error('LOCHO_HOST_BINARY is required');
const directory = await mkdtemp(join(process.env.TMPDIR ?? tmpdir(), 'locho-service-comparison-'));
await chmod(directory, 0o700);
const hostSha256 = sha256(await readFile(process.env.LOCHO_HOST_BINARY));
const configurations = [
  { name: 'unchanged_package', sdk: 'package', trace: 'none', hostTrace: 'milestones' },
  { name: 'instrumented_disabled', sdk: 'instrumented', trace: 'none', hostTrace: 'milestones' },
  { name: 'instrumented_milestones', sdk: 'instrumented', trace: 'milestones', hostTrace: 'milestones' },
  { name: 'instrumented_frames', sdk: 'instrumented', trace: 'transport', hostTrace: 'transport' },
];
const report = { schema: 1, purpose: 'bounded-diagnostic-comparison', releaseGate: false, maxCasesPerConfiguration: 30,
  hostSha256, startedAt: new Date().toISOString(), configurations: [] };
for (const configuration of configurations) {
  if (sha256(await readFile(process.env.LOCHO_HOST_BINARY)) !== hostSha256) throw new Error('Host binary changed between configurations');
  console.log(`Configuration: ${configuration.name} (maximum 30 cases, stop on first failure)`);
  const { status, output } = await new Promise((resolveResult, reject) => {
    const child = spawn(process.execPath, [resolve(pkg, 'scripts/reproduce-service-setup.mjs'), '--mode', 'n0', '--browser', 'chromium',
      '--cycles', '10', '--sdk', configuration.sdk, '--trace', configuration.trace, '--host-trace', configuration.hostTrace],
    { cwd: pkg, env: process.env, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '';
    child.stdout.on('data', (bytes) => { output += bytes.toString(); process.stdout.write(bytes); });
    // Do not forward raw exception output; the runner's private summary carries
    // allowlisted failures, and nothing should echo fixture arguments/credentials.
    child.stderr.resume();
    child.once('error', reject);
    child.once('exit', (status, signal) => resolveResult({ status: signal ? null : status, output }));
  });
  const evidence = output.match(/private evidence: ([^\n]+)/)?.[1];
  if (!evidence) { report.configurations.push({ ...configuration, status, harnessFailure: true }); break; }
  const summary = JSON.parse(await readFile(join(evidence, 'summary.json'), 'utf8'));
  report.configurations.push({ ...configuration, status, evidence, summary });
  await writeFile(join(directory, 'comparison.json'), JSON.stringify(report, null, 2), { mode: 0o600 });
  // Different configurations are planned experiments, not retries of failed cases.
  // Stop the matrix on invalid provenance/capture setup rather than reinterpret it.
  if (summary.harnessFailure || summary.inputsStable !== true || summary.credentialScan.rejectedFiles > 0) break;
}
report.finishedAt = new Date().toISOString();
report.complete = report.configurations.length === configurations.length;
await writeFile(join(directory, 'comparison.json'), JSON.stringify(report, null, 2), { mode: 0o600 });
console.log(`Comparison evidence: ${directory}`);
if (!report.complete || report.configurations.some((entry) => entry.summary?.failed || entry.harnessFailure)) process.exitCode = 1;
