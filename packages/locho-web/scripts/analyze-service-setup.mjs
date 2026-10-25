import { readFile, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { traceEvent } from './repro-trace-events.mjs';

if (process.argv.length !== 3) throw new Error('Expected private reproduction directory');
const directory = resolve(process.argv[2]);
const summary = JSON.parse(await readFile(join(directory, 'summary.json'), 'utf8'));
if (!summary.credentialScan?.inputsAvailable || summary.credentialScan.rejectedFiles > 0) {
  throw new Error('Refusing to analyze unscanned or rejected raw evidence');
}
const host = await readFile(join(directory, 'host.log'));
const analysis = { schema: 1, diagnosticOnly: true, inputsStable: summary.inputsStable,
  traceCoverage: summary.traceCoverage, cases: [] };
for (const result of summary.results) {
  if (!result.hostTraceRange) continue;
  const range = result.hostTraceRange;
  const events = [];
  const hostLines = host.subarray(range.start, range.end).toString().split('\n');
  for (const [index, line] of hostLines.entries()) {
    const event = traceEvent(line);
    if (event) events.push({ side: 'host', line: index, ...event });
  }
  const browserText = await readFile(join(directory, `${result.browser}-${result.case}.log`), 'utf8');
  for (const [index, line] of browserText.split('\n').filter(Boolean).entries()) {
    let record;
    try { record = JSON.parse(line); } catch { continue; }
    const event = traceEvent(record.line);
    if (event) events.push({ side: 'browser', line: index, atUnixMs: record.atUnixMs, ...event });
  }
  const counts = {};
  for (const event of events) {
    const key = `${event.side}:${event.stage ?? event.kind}${event.direction ? `:${event.direction}` : ''}`;
    counts[key] = (counts[key] ?? 0) + 1;
  }
  analysis.cases.push({ case: result.case, scenario: result.scenario, passed: result.passed, counts,
    // Retain full metadata for failures only; successful cases need just counts.
    ...(!result.passed ? { events } : {}) });
}
await writeFile(join(directory, 'analysis.json'), JSON.stringify(analysis, null, 2), { mode: 0o600 });
console.log(JSON.stringify({ cases: analysis.cases.length, failures: analysis.cases.filter((entry) => !entry.passed),
  traceCoverage: analysis.traceCoverage }, null, 2));
