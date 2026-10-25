// Investigation harness, NOT a release acceptance gate or retry wrapper.
import { chromium, firefox, webkit } from '@playwright/test';
import { createServer } from 'node:http';
import { createServer as createTcpServer, connect as connectTcp } from 'node:net';
import { Transform } from 'node:stream';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm, chmod } from 'node:fs/promises';
import { openSync, writeSync, closeSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { resolve, join, extname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';
import { sha256, treeHashes, wasmSourceHashes, sameHashes, fileHashes } from './repro-provenance.mjs';
import { hostTraceFilter } from './repro-trace-filter.mjs';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const pkg = resolve(root, 'packages/locho-web');
const args = process.argv.slice(2);
const knownOptions = new Set(['--mode', '--browser', '--cycles', '--scenario', '--trace', '--host-trace', '--host-buffered', '--host-snapshots', '--sdk', '--fault-delay-ms', '--auth-reply-delay-ms', '--relay-layout', '--relay-delay-ms', '--home-delay-ms', '--relay-aliases', '--alias-first-delay-ms', '--alias-release-on-upstream', '--canonical-home', '--canonical-n0-home']);
if (args.length % 2 || args.some((value, index) => index % 2 === 0 && !knownOptions.has(value))) throw new Error('Invalid reproduction options');
const option = (name, fallback) => {
  const index = args.indexOf(name);
  return index < 0 ? fallback : args[index + 1];
};
const mode = option('--mode', 'controlled');
const engine = option('--browser', 'chromium');
const cycles = Number(option('--cycles', '10'));
const scenarioFilter = option('--scenario', 'all');
const trace = option('--trace', 'milestones');
const hostTrace = option('--host-trace', trace);
const hostBuffered = option('--host-buffered', '0');
const hostSnapshots = option('--host-snapshots', '0');
const snapshotsActive = hostBuffered === '1' || hostSnapshots === '1';
const sdk = option('--sdk', 'instrumented');
const faultDelayMs = Number(option('--fault-delay-ms', '0'));
const authReplyDelayMs = Number(option('--auth-reply-delay-ms', '0'));
const relayLayout = option('--relay-layout', 'same');
const relayDelayMs = Number(option('--relay-delay-ms', '0'));
const homeDelayMs = Number(option('--home-delay-ms', '0'));
const relayAliases = option('--relay-aliases', '0');
const canonicalHome = option('--canonical-home', '0');
const canonicalN0Home = option('--canonical-n0-home', '0');
const aliasFirstDelayMs = Number(option('--alias-first-delay-ms', '0'));
const aliasReleaseOnUpstream = option('--alias-release-on-upstream', '0');
if (!['controlled', 'n0'].includes(mode) || !['all', 'chromium', 'firefox', 'webkit'].includes(engine)
    || !['all', 'explicit', 'stale', 'stale_delayed'].includes(scenarioFilter)
    || !['none', 'milestones', 'transport'].includes(trace)
    || !['none', 'milestones', 'transport', 'frames'].includes(hostTrace) || !Number.isInteger(cycles) || cycles < 1 || cycles > 1000
    || !['0', '1'].includes(hostBuffered) || (hostBuffered === '1' && (mode !== 'n0' || process.platform === 'win32'))
    || !['0', '1'].includes(hostSnapshots) || (hostSnapshots === '1' && (mode !== 'n0' || process.platform === 'win32' || hostBuffered === '1'))
    || ![0, 20_000].includes(faultDelayMs) || (faultDelayMs > 0 && mode !== 'controlled')
    || !Number.isInteger(authReplyDelayMs) || authReplyDelayMs < 0 || authReplyDelayMs > 5000
    || (authReplyDelayMs > 0 && (mode !== 'controlled' || faultDelayMs > 0))
    || !['package', 'instrumented'].includes(sdk) || (sdk === 'package' && mode !== 'n0')
    || !['same', 'split'].includes(relayLayout) || ![relayDelayMs, homeDelayMs].every((value) => Number.isInteger(value) && value >= 0 && value <= 1000)
    || (relayLayout === 'same' && homeDelayMs > 0)
    || !['0', '1'].includes(relayAliases) || !['0', '1'].includes(canonicalHome)
    || (relayAliases === '1' && (mode !== 'controlled' || relayLayout !== 'same'))
    || (canonicalHome === '1' && relayAliases !== '1')
    || !['0', '1'].includes(canonicalN0Home) || (canonicalN0Home === '1' && (mode !== 'n0' || sdk !== 'instrumented'))
    || !Number.isInteger(aliasFirstDelayMs) || aliasFirstDelayMs < 0 || aliasFirstDelayMs > 5000
    || (aliasFirstDelayMs > 0 && relayAliases !== '1')
    || !['0', '1'].includes(aliasReleaseOnUpstream)
    || (aliasReleaseOnUpstream === '1' && (relayAliases !== '1' || aliasFirstDelayMs > 0 || authReplyDelayMs === 0))
    || (mode === 'n0' && (relayLayout !== 'same' || relayDelayMs || homeDelayMs))) {
  throw new Error('Invalid reproduction options');
}
const directory = await mkdtemp(join(process.env.TMPDIR ?? tmpdir(), 'locho-service-repro-'));
await chmod(directory, 0o700);
const state = join(directory, 'state'); await mkdir(state, { mode: 0o700 });
const privateTraces = [];
const traceStats = {};
const MAX_TRACE = 16 * 1024 * 1024;
function recorder(name) {
  const path = join(directory, `${name}.log`);
  const fd = openSync(path, 'wx', 0o600);
  privateTraces.push(path);
  const stats = traceStats[name] = { bytes: 0, droppedBytes: 0, truncatedLines: 0 };
  return { append(value) {
    if (String(value).includes('[trace_line_truncated bytes=')) stats.truncatedLines++;
    const bytes = Buffer.from(value);
    const count = Math.min(bytes.length, MAX_TRACE - stats.bytes);
    if (count > 0) { writeSync(fd, bytes.subarray(0, count)); stats.bytes += count; }
    stats.droppedBytes += bytes.length - count;
  }, close() { closeSync(fd); } };
}
function listen(server) {
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => resolve(server.address().port));
  });
}
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
function discoveryUrlFor(id) {
  const alphabet = 'ybndrfg8ejkmcpqxot1uwisza345h769';
  let value = 0, bits = 0, encoded = '';
  for (const byte of Buffer.from(id, 'hex')) {
    value = (value << 8) | byte; bits += 8;
    while (bits >= 5) { bits -= 5; encoded += alphabet[(value >>> bits) & 31]; }
  }
  if (bits) encoded += alphabet[(value << (5 - bits)) & 31];
  return `https://dns.iroh.link/pkarr/${encoded}`;
}
async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolve) => {
    const timer = setTimeout(() => child.kill('SIGKILL'), 7000);
    child.once('exit', () => { clearTimeout(timer); resolve(); });
    child.kill('SIGTERM');
  });
}
const requests = new Map();
let tcpConnections = 0;
let activeSockets = 0;
let aliasCaseUpstreams = 0;
const aliasWaiters = new Set();
const upstreamEvents = [];
const upstream = createServer((request, response) => {
  const authority = /^repro-\d+-[01]$/.test(request.headers.host ?? '') ? request.headers.host : 'unexpected';
  requests.set(authority, (requests.get(authority) ?? 0) + 1);
  upstreamEvents.push({ atUnixMs: Date.now(), stage: 'http_received', authority });
  response.writeHead(200, { Connection: 'close', 'Content-Length': Buffer.byteLength('locho-repro-health\n') });
  response.end('locho-repro-health\n');
});
upstream.on('connection', (socket) => {
  const id = ++tcpConnections;
  activeSockets++;
  aliasCaseUpstreams++;
  if (aliasCaseUpstreams >= 2) for (const release of [...aliasWaiters]) release(false);
  upstreamEvents.push({ atUnixMs: Date.now(), stage: 'tcp_connected', id });
  socket.on('close', () => { activeSockets--; upstreamEvents.push({ atUnixMs: Date.now(), stage: 'tcp_closed', id }); });
});
const browserFiles = resolve(pkg, 'dist/repro/browser');
let packageFiles;
const files = createServer(async (request, response) => {
  try {
    const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    if (sdk === 'package' && pathname === '/') {
      response.writeHead(200, { 'Content-Type': 'text/html' });
      response.end('<!doctype html><title>Disposable packaged SDK baseline</title><script type="module">import {LochoClient,initializeLocho} from "/sdk/index.js";import {installRunner} from "/runner.js";installRunner({LochoClient,initializeLocho});</script>');
      return;
    }
    if (sdk === 'package' && pathname === '/runner.js') {
      response.writeHead(200, { 'Content-Type': 'text/javascript' });
      response.end(await readFile(resolve(pkg, 'tests/repro-page/runner.js'))); return;
    }
    const base = sdk === 'package' && pathname.startsWith('/sdk/') ? packageFiles : browserFiles;
    const relative = base === packageFiles ? pathname.slice(4) : pathname === '/' ? '/index.html' : pathname;
    const path = resolve(base, `.${relative}`);
    if (!path.startsWith(`${base}/`)) throw new Error('invalidPath');
    const content = await readFile(path);
    const mime = { '.js': 'text/javascript', '.html': 'text/html', '.wasm': 'application/wasm' }[extname(path)] ?? 'application/octet-stream';
    response.writeHead(200, { 'Content-Type': mime }); response.end(content);
  } catch { response.writeHead(404); response.end(); }
});
let child;
let hostLog;
let hostId;
let relayUrl;
let homeRelayUrl;
let secrets = [];
let failed = false;
const errorCodes = new Set(['harnessTimeout', 'hostStartupTimeout', 'hostSpawnFailed', 'hostExited', 'publicRelayNotReady',
  'publicDiscoveryNotReady', 'baselineIntegrityMismatch', 'baselineNotClean', 'transportLockMismatch', 'instrumentedBuildMismatch', 'bufferDumpTimeout', 'snapshotHostUnsupported', 'LOCHO_HOST_BINARY required for N0 mode']);
const failureCode = (error) => errorCodes.has(error?.message) ? error.message : error?.code === 'ENOENT' ? 'missingInput' : 'internalHarness';
const results = [];
const relayProxyEvents = [];
const proxies = [];
const relayFile = join(directory, 'relay.json');
async function makeProxy(field, delayMs) {
  const sockets = new Set();
  let ordinal = 0;
  let browserConnections = 0;
  const server = createTcpServer(async (front) => {
    const id = ordinal++;
    sockets.add(front); front.pause();
    let back;
    try {
      let target;
      for (let attempt = 0; attempt < 100 && !target; attempt++) {
        try { target = JSON.parse(await readFile(relayFile, 'utf8'))[field]; } catch {}
        if (!target) await sleep(20);
      }
      const url = new URL(target);
      if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1' || !url.port) throw new Error('invalidProxyTarget');
      back = connectTcp({ host: '127.0.0.1', port: Number(url.port) }); sockets.add(back);
      const totals = { up: 0, down: 0 };
      let header = Buffer.alloc(0);
      let headerDone = false;
      const delayed = (direction) => new Transform({ transform(chunk, _encoding, callback) {
        totals[direction] += chunk.length;
        if (relayAliases === '1' && (aliasFirstDelayMs > 0 || aliasReleaseOnUpstream === '1') && direction === 'up' && !headerDone) {
          // Only inspect disposable loopback HTTP upgrade headers, in memory.
          // Never retain their text. Delay the first two browser relay upgrades
          // equally in BOTH alias/canonical controls, independent of spelling.
          header = Buffer.concat([header, chunk]);
          if (!header.includes('\r\n\r\n') && header.length < 8192) { callback(); return; }
          headerDone = true;
          const browser = /(?:^|\r\n)host:\s*localhost\.?(?::\d+)?\s*\r\n/i.test(header.toString('latin1'));
          const dotted = /(?:^|\r\n)host:\s*localhost\.(?::\d+)?\s*\r\n/i.test(header.toString('latin1'));
          const browserOrdinal = browser ? browserConnections++ : null;
          const initialDelayMs = browser && browserOrdinal < 2 ? aliasFirstDelayMs : 0;
          relayProxyEvents.push({ atUnixMs: Date.now(), relay: field, id, stage: 'upgrade_classified', browser, dotted, browserOrdinal, initialDelayMs });
          const bytes = header; header = Buffer.alloc(0);
          if (aliasReleaseOnUpstream === '1' && dotted && aliasCaseUpstreams < 2) {
            let released = false;
            let timer;
            const release = (timedOut = false) => {
              if (released) return;
              released = true; clearTimeout(timer); aliasWaiters.delete(release);
              relayProxyEvents.push({ atUnixMs: Date.now(), relay: field, id, stage: 'upgrade_released', timedOut, upstreamCount: aliasCaseUpstreams });
              callback(null, bytes);
            };
            aliasWaiters.add(release);
            timer = setTimeout(() => release(true), 8000);
            front.once('close', () => release(true));
            return;
          }
          if (delayMs + initialDelayMs) setTimeout(() => callback(null, bytes), delayMs + initialDelayMs);
          else callback(null, bytes);
          return;
        }
        if (delayMs) setTimeout(() => callback(null, chunk), delayMs);
        else callback(null, chunk);
      } });
      const up = delayed('up'), down = delayed('down');
      const close = () => { front.destroy(); back.destroy(); up.destroy(); down.destroy(); };
      up.on('error', close); down.on('error', close);
      front.on('error', close); back.on('error', close);
      front.on('close', () => { sockets.delete(front); close(); relayProxyEvents.push({ atUnixMs: Date.now(), relay: field, id, stage: 'closed', ...totals }); });
      back.on('close', () => { sockets.delete(back); front.destroy(); });
      relayProxyEvents.push({ atUnixMs: Date.now(), relay: field, id, stage: 'opened' });
      front.pipe(up).pipe(back); back.pipe(down).pipe(front); front.resume();
    } catch { front.destroy(); back?.destroy(); relayProxyEvents.push({ atUnixMs: Date.now(), relay: field, id, stage: 'failed' }); }
  });
  const port = await listen(server);
  proxies.push({ server, sockets, reset: () => { browserConnections = 0; } });
  return `http://127.0.0.1:${port}/`;
}
const summary = { mode, sdk, trace, hostTrace, scenarioFilter, hostBuffered: hostBuffered === '1', hostSnapshots: hostSnapshots === '1', browserTracingAvailable: sdk === 'instrumented', cycles, relayLayout, relayDelayMs, homeDelayMs,
  relayAliases: relayAliases === '1', canonicalHome: canonicalHome === '1',
  canonicalN0Home: canonicalN0Home === '1',
  aliasFirstDelayMs,
  aliasReleaseOnUpstream: aliasReleaseOnUpstream === '1',
  faultDelayMs, authReplyDelayMs, expectedFault: faultDelayMs > 0, productionArtifact: false, sourceRevision: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
  sourceDirty: Boolean(execFileSync('git', ['status', '--porcelain'], { cwd: root, encoding: 'utf8' }).trim()), results, traceStats };
let frozenInputs;
const bufferedBinary = resolve(root, 'target/browser-repro/debug/n0-trace');
const bufferDump = join(directory, 'host-buffer.json');
let bufferSequence = 0;
let bufferDroppedBytes = 0;
let snapshotsReady = false;
async function drainHostBufferOnce() {
  if (!snapshotsReady || !child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGUSR1');
  for (let attempt = 0; attempt < 250; attempt++) {
    try {
      const snapshot = JSON.parse(await readFile(bufferDump, 'utf8'));
      if (snapshot.sequence === bufferSequence + 1 && typeof snapshot.data === 'string'
        && Number.isSafeInteger(snapshot.dropped_bytes) && snapshot.dropped_bytes >= 0) {
        bufferSequence = snapshot.sequence;
        bufferDroppedBytes += snapshot.dropped_bytes;
        hostLog.append(snapshot.data);
        return snapshot.connections;
      }
    } catch {}
    await sleep(20);
  }
  throw new Error('bufferDumpTimeout');
}
let bufferDrainChain = Promise.resolve();
function drainHostBuffer() {
  bufferDrainChain = bufferDrainChain.then(drainHostBufferOnce);
  return bufferDrainChain;
}
async function currentInputs(binary) {
  return {
    hostSha256: sha256(await readFile(binary)),
    runner: await fileHashes(pkg, ['scripts/reproduce-service-setup.mjs', 'scripts/repro-provenance.mjs', 'scripts/repro-trace-filter.mjs', 'tests/repro-page/runner.js']),
    ...(hostSnapshots === '1' ? { snapshotHostSources: await fileHashes(root,
      ['src/main.rs', 'src/host.rs', 'Cargo.toml', 'Cargo.lock']) } : {}),
    ...(hostBuffered === '1' ? { bufferedHostSha256: sha256(await readFile(bufferedBinary)),
      bufferedHostSources: await fileHashes(root, ['tests/browser-repro/Cargo.toml', 'tests/browser-repro/Cargo.lock',
        'tests/browser-repro/src/bin/n0-trace.rs', 'tests/browser-repro/src/buffered_trace.rs',
        'src/auth.rs', 'src/config.rs', 'src/host.rs', 'src/http_utils.rs', 'src/relay.rs', 'src/state.rs', 'src/task.rs']),
    } : {}),
    ...(sdk === 'instrumented' ? {
      generated: await treeHashes(resolve(pkg, 'dist/repro/generated')),
      browser: await treeHashes(browserFiles), sources: await wasmSourceHashes(root),
    } : { archiveSha256: sha256(await readFile(resolve(pkg, 'dist/artifacts/locho_web-0.1.0-beta.3.tgz'))) }),
  };
}
let binary;
try {
  summary.startedAt = new Date().toISOString();
  summary.nodeVersion = process.version;
  if (sdk === 'instrumented') {
    const provenance = JSON.parse(await readFile(resolve(pkg, 'dist/repro/build.json'), 'utf8'));
    if (provenance.schema !== 1 || !sameHashes(provenance.generated, await treeHashes(resolve(pkg, 'dist/repro/generated')))
      || !sameHashes(provenance.browser, await treeHashes(browserFiles))
      || !sameHashes(provenance.sourceHashes, await wasmSourceHashes(root))) throw new Error('instrumentedBuildMismatch');
    summary.instrumentedBuild = provenance;
  }
  if (sdk === 'package') {
    const archive = resolve(pkg, 'dist/artifacts/locho_web-0.1.0-beta.3.tgz');
    const manifest = JSON.parse(await readFile(resolve(pkg, 'dist/artifacts/pack-manifest.json'), 'utf8'));
    const integrity = `sha512-${createHash('sha512').update(await readFile(archive)).digest('base64')}`;
    if (manifest.version !== '0.1.0-beta.3' || integrity !== manifest.integrity) throw new Error('baselineIntegrityMismatch');
    const extracted = join(directory, 'baseline'); await mkdir(extracted, { mode: 0o700 });
    execFileSync('python3', ['-c', 'import sys,tarfile; tarfile.open(sys.argv[1]).extractall(sys.argv[2], filter="data")', archive, extracted], { stdio: 'pipe' });
    packageFiles = join(extracted, 'package/dist/package');
    const provenance = JSON.parse(await readFile(join(packageFiles, 'build.json'), 'utf8'));
    if (provenance.version !== '0.1.0-beta.3' || provenance.dirty !== false) throw new Error('baselineNotClean');
    summary.baselinePackage = { integrity, provenance };
  }
  const names = new Set(['iroh', 'iroh-base', 'iroh-relay', 'noq', 'noq-proto', 'noq-udp', 'tokio', 'ctutils', 'rustls', 'rustls-webpki', 'n0-watcher']);
  const versions = (text) => text.split('[[package]]').flatMap((block) => {
    const name = block.match(/^name = "([^"]+)"/m)?.[1];
    const version = block.match(/^version = "([^"]+)"/m)?.[1];
    return names.has(name) ? [`${name}@${version}`] : [];
  }).sort();
  const lock = await readFile(resolve(root, 'Cargo.lock'), 'utf8');
  summary.transportVersions = versions(lock);
  summary.workspaceLockSha256 = createHash('sha256').update(lock).digest('hex');
  if (mode === 'controlled') {
    const fixtureLock = await readFile(resolve(root, 'tests/browser-repro/Cargo.lock'), 'utf8');
    summary.fixtureTransportVersions = versions(fixtureLock);
    summary.fixtureLockSha256 = createHash('sha256').update(fixtureLock).digest('hex');
    if (JSON.stringify(summary.transportVersions) !== JSON.stringify(summary.fixtureTransportVersions)) throw new Error('transportLockMismatch');
  }
  const port = await listen(upstream);
  const browserPort = await listen(files);
  const config = join(directory, 'host.toml');
  await writeFile(config, `[[services]]\nname = "sample-api"\ntype = "tcp"\nendpoint = "127.0.0.1:${port}"\n`, { mode: 0o600 });
  binary = mode === 'controlled' ? resolve(root, 'target/browser-repro/debug/locho-browser-repro') : process.env.LOCHO_HOST_BINARY;
  if (!binary) throw new Error('LOCHO_HOST_BINARY required for N0 mode');
  frozenInputs = await currentInputs(binary);
  summary.frozenInputs = frozenInputs;
  if (mode === 'n0') summary.hostVersion = execFileSync(binary, ['--version'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  let hostProxy, browserProxy;
  if (mode === 'controlled') {
    hostProxy = await makeProxy('backendRelayUrl', relayDelayMs);
    browserProxy = relayLayout === 'split' ? await makeProxy('browserBackendRelayUrl', homeDelayMs) : hostProxy;
    if (relayAliases === '1') { const alias = new URL(browserProxy); alias.hostname = 'localhost.'; browserProxy = alias.toString(); }
  }
  hostLog = recorder('host');
  const filter = hostTraceFilter(hostTrace);
  summary.hostTraceFilter = filter;
  const hostEnv = { ...process.env, LOCHO_STATE_DIR: state, RUST_LOG: filter, LOCHO_TEST_AUTH_REPLY_DELAY_MS: String(faultDelayMs || authReplyDelayMs) };
  delete hostEnv.LOCHO_TEST_BIND_ADDR; delete hostEnv.LOCHO_TEST_TCP_CONNECT_TIMEOUT_MS;
  delete hostEnv.LOCHO_TEST_CONNECTION_SNAPSHOT_FILE;
  if (hostSnapshots === '1') hostEnv.LOCHO_TEST_CONNECTION_SNAPSHOT_FILE = bufferDump;
  child = spawn(hostBuffered === '1' ? bufferedBinary : binary,
    hostBuffered === '1' ? [config, bufferDump] : mode === 'controlled' ? [config, relayFile, hostProxy, browserProxy, relayLayout] : ['host', '--config', config], {
    env: hostEnv, stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stderr.on('data', (bytes) => hostLog.append(bytes));
  // The production CLI's tracing subscriber writes to stdout; the standalone
  // fixture writes to stderr. Capture BOTH privately, never echo either stream.
  child.stdout.on('data', (bytes) => hostLog.append(bytes));
  await new Promise((resolve, reject) => {
    let output = '';
    const timer = setTimeout(() => reject(new Error('hostStartupTimeout')), 30_000);
    child.once('error', () => { clearTimeout(timer); reject(new Error('hostSpawnFailed')); });
    child.once('exit', () => { clearTimeout(timer); reject(new Error('hostExited')); });
    child.stdout.on('data', (bytes) => {
      output = (output + bytes.toString()).slice(-8192);
      const match = output.match(/host id:\s*([0-9a-f]{64})/i);
      if (match) { hostId = match[1]; clearTimeout(timer); resolve(); }
    });
  });
  if (snapshotsActive) {
    try { await readFile(bufferDump); } catch { throw new Error('snapshotHostUnsupported'); }
    snapshotsReady = true;
  }
  const credentials = JSON.parse(await readFile(join(state, 'host_state.json'), 'utf8'));
  const secret = credentials.service_secrets['sample-api'];
  const capability = `sample-api:tcp:${secret}`;
  const proof = createHash('sha256').update(`locho-v1:${secret}`).digest('hex');
  const key = await readFile(join(state, 'host.key'));
  secrets = [secret, proof, capability, credentials.attach_secret, key.toString('hex'), key.toString('base64'), [...key].join(', ')]
    .filter((value) => typeof value === 'string' && value.length >= 16);
  if (mode === 'controlled') {
    const manifest = JSON.parse(await readFile(relayFile, 'utf8'));
    relayUrl = manifest.relayUrl; homeRelayUrl = manifest.homeRelayUrl;
    if (relayAliases === '1') { const alias = new URL(relayUrl); alias.hostname = 'localhost'; relayUrl = alias.toString(); }
  }
  else {
    for (let attempt = 0; attempt < 12 && !relayUrl; attempt++) {
      try {
        const diagnostic = execFileSync(binary, ['diagnose', '--host-id', hostId], { cwd: root,
          env: { ...process.env, LOCHO_STATE_DIR: state, RUST_LOG: 'off' }, encoding: 'utf8', timeout: 20_000, stdio: ['ignore', 'pipe', 'pipe'] });
        relayUrl = diagnostic.match(/relay:\s+(https:\/\/[^)\s]+)/)?.[1];
      } catch {}
      if (!relayUrl) await sleep(1000);
    }
    if (!relayUrl) throw new Error('publicRelayNotReady');
    const url = new URL(relayUrl); url.hostname = url.hostname.replace(/\.$/, ''); relayUrl = url.toString();
    let discoveryReady = false;
    for (let attempt = 0; attempt < 12 && !discoveryReady; attempt++) {
      try { discoveryReady = (await fetch(discoveryUrlFor(hostId), { signal: AbortSignal.timeout(5000), cache: 'no-store' })).ok; } catch {}
      if (!discoveryReady) await sleep(1000);
    }
    if (!discoveryReady) throw new Error('publicDiscoveryNotReady');
  }
  const scenarios = [{ name: 'explicit', stale: false, delay: 0 }, { name: 'stale', stale: true, delay: 0 }, { name: 'stale_delayed', stale: true, delay: 750 }]
    .filter((scenario) => scenarioFilter === 'all' || scenario.name === scenarioFilter);
  summary.peerRelayTag = sha256(relayUrl).slice(0, 12);
  summary.connectPath = mode === 'n0' ? 'public-js-connect' : 'controlled-private-connect';
  let caseId = 0;
  for (const [name, browserType] of Object.entries({ chromium, firefox, webkit })) {
    if (engine !== 'all' && engine !== name) continue;
    const browser = await browserType.launch();
    try {
      for (let cycle = 0; cycle < cycles && !failed; cycle++) for (const scenario of scenarios) {
        const id = caseId++;
        aliasCaseUpstreams = 0;
        for (const proxy of proxies) proxy.reset();
        const page = await browser.newPage();
        const websocketGroups = new Map();
        if (relayAliases === '1') page.on('websocket', (socket) => {
          const url = new URL(socket.url());
          const dotted = url.hostname.endsWith('.');
          const tag = sha256(`${url.protocol}//${url.hostname.replace(/\.$/, '')}:${url.port}${url.pathname}`).slice(0, 12);
          const counts = websocketGroups.get(tag) ?? { relayTag: tag, dotted: 0, plain: 0, closed: 0, errors: 0 };
          counts[dotted ? 'dotted' : 'plain']++;
          websocketGroups.set(tag, counts);
          socket.on('close', () => { counts.closed++; });
          socket.on('socketerror', () => { counts.errors++; });
        });
        const browserLog = recorder(`${name}-${id}`);
        await page.exposeFunction('__lochoReproTrace', (line) => browserLog.append(`${JSON.stringify({ atUnixMs: Date.now(), line: String(line) })}\n`));
        if (snapshotsActive) await page.exposeFunction('__lochoReproFailure', () => drainHostBuffer());
        try {
          await page.goto(`http://127.0.0.1:${browserPort}/`);
          await page.waitForFunction(() => Boolean(window.lochoRepro));
          if (mode === 'n0' && scenario.delay) await page.route(discoveryUrlFor(hostId), async (route) => {
            await sleep(scenario.delay); await route.continue();
          });
          const before = tcpConnections;
          const hostTraceStart = traceStats.host.bytes;
          let timer;
          const clients = await Promise.race([
            page.evaluate((options) => window.lochoRepro(options), { mode, hostId, relayUrl, homeRelayUrl, capability, stale: scenario.stale,
              lookupDelayMs: scenario.delay, trace, cycle: id, canonicalHome: canonicalHome === '1', canonicalN0Home: canonicalN0Home === '1' }),
            new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('harnessTimeout')), 65_000); }),
          ]).finally(() => clearTimeout(timer));
          const upstreamRequests = [0, 1].map((index) => requests.get(`repro-${id}-${index}`) ?? 0);
          for (let attempt = 0; attempt < 50 && activeSockets > 0; attempt++) await sleep(100);
          await drainHostBuffer();
          const ok = clients.every((client) => client.status === 200 && client.correctBody && client.path === 'relay'
            && !client.timeline.some((event) => event.stage === 'close_failed'))
            && upstreamRequests.every((count) => count === 1) && activeSockets === 0;
          const faultObserved = faultDelayMs > 0 && clients.every((client) => client.stage === 'health' && client.errorCode === 'timeout')
            && tcpConnections - before === 2 && upstreamRequests.every((count) => count === 0) && activeSockets === 0;
          results.push({ browser: name, browserVersion: browser.version(), case: id, cycle, scenario: scenario.name,
            clients, upstreamRequests, tcpConnections: tcpConnections - before, activeSockets, passed: ok, faultObserved,
            websocketGroups: JSON.parse(JSON.stringify([...websocketGroups.values()])),
            hostTraceRange: { start: hostTraceStart, end: traceStats.host.bytes } });
          console.log(`${name} ${scenario.name} ${cycle}: ${faultObserved ? 'EXPECTED AUTHORIZATION TIMEOUT' : ok ? 'pass' : 'FAIL'}`);
          if (faultDelayMs > 0 ? !faultObserved : !ok) { failed = true; break; }
        } catch (error) { results.push({ browser: name, case: id, scenario: scenario.name, passed: false, harnessFailure: true, code: failureCode(error) }); failed = true; break; }
        finally { await page.close(); browserLog.close(); }
      }
    } finally { await browser.close(); }
    if (failed) break;
  }
} catch (error) { failed = true; summary.harnessFailure = true; summary.code = failureCode(error); }
finally {
  try { await drainHostBuffer(); } catch { failed = true; summary.bufferDumpFailed = true; }
  await stop(child); hostLog?.close();
  for (const { server, sockets } of proxies) {
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => server.close(resolve));
  }
  upstream.closeAllConnections(); await new Promise((resolve) => upstream.close(resolve));
  files.closeAllConnections(); await new Promise((resolve) => files.close(resolve));
  if (frozenInputs) {
    try { summary.inputsStable = sameHashes(frozenInputs, await currentInputs(binary)); }
    catch { summary.inputsStable = false; }
    if (!summary.inputsStable) failed = true;
  }
  summary.finishedAt = new Date().toISOString();
  const variants = secrets.flatMap((value) => [value, value.toUpperCase(), Buffer.from(value).toString('base64'), Buffer.from(value).toString('hex')]);
  let rejected = 0;
  summary.stageCounts = {};
  for (const path of privateTraces) {
    const bytes = await readFile(path);
    const name = path.slice(path.lastIndexOf('/') + 1);
    let text = bytes.toString();
    if (name !== 'host.log') text = text.split('\n').filter(Boolean).flatMap((line) => {
      try { return [JSON.parse(line).line]; } catch { return []; }
    }).join('\n');
    const counts = summary.stageCounts[name] = {};
    const allowed = new Set(['authorization_request_start', 'authorization_request_written', 'authorization_reply_decoded',
      'head_prefix_read', 'head_bytes_read', 'upstream_connected', 'authorization_reply_start', 'authorization_reply_written', 'connect_return']);
    for (const match of text.matchAll(/stage="?([a-z_]+)/g)) if (allowed.has(match[1])) counts[match[1]] = (counts[match[1]] ?? 0) + 1;
    if (variants.length === 0 || variants.some((value) => bytes.includes(Buffer.from(value)))) { await rm(path); rejected++; }
  }
  summary.credentialScan = { rejectedFiles: rejected, checkedFiles: privateTraces.length, inputsAvailable: variants.length > 0 };
  summary.traceCoverage = {
    hostMilestones: Object.values(summary.stageCounts['host.log'] ?? {}).reduce((sum, value) => sum + value, 0),
    browserMilestones: Object.entries(summary.stageCounts).filter(([name]) => name !== 'host.log')
      .reduce((sum, [, counts]) => sum + Object.values(counts).reduce((total, value) => total + value, 0), 0),
    truncated: bufferDroppedBytes > 0 || Object.values(traceStats).some((stats) => stats.droppedBytes > 0 || stats.truncatedLines > 0),
    hostAuthorizationReplies: summary.stageCounts['host.log']?.authorization_reply_written ?? 0,
    expectedSuccessfulReplies: results.reduce((sum, result) => sum + (result.clients?.filter((client) => client.status === 200).length ?? 0), 0),
  };
  summary.traceCoverage.missingSuccessfulHostReplies = hostTrace === 'none' ? null : Math.max(0,
    summary.traceCoverage.expectedSuccessfulReplies - summary.traceCoverage.hostAuthorizationReplies);
  summary.bufferedTrace = { dumps: bufferSequence, droppedBytes: bufferDroppedBytes };
  summary.failed = failed || rejected > 0;
  summary.healthy = !summary.failed && faultDelayMs === 0;
  summary.upstreamEvents = upstreamEvents;
  summary.relayProxyEvents = relayProxyEvents;
  await writeFile(join(directory, 'summary.json'), JSON.stringify(summary, null, 2), { mode: 0o600 });
  await rm(state, { recursive: true, force: true });
  await rm(join(directory, 'host.toml'), { force: true });
  await rm(join(directory, 'relay.json'), { force: true });
  await rm(join(directory, 'baseline'), { recursive: true, force: true });
  await rm(bufferDump, { force: true });
  console.log(`Reproduction ${summary.failed ? 'FAILED' : faultDelayMs > 0 ? 'expected fault observed (not a healthy run)' : 'passed'}; private evidence: ${directory}`);
  if (summary.failed) process.exitCode = 1;
}
