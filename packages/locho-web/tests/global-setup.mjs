import { createServer } from 'node:http';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const execFileAsync = promisify(execFile);
const BODY = 'locho-n0-browser-health\n';
const SERVICE = 'sample-api';

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

function listen(server) {
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      server.off('error', reject);
      resolve(server.address().port);
    });
  });
}

function waitForHost(child) {
  return new Promise((resolve, reject) => {
    let output = '';
    const timer = setTimeout(() => {
      finish(new Error(`timed out waiting for locho host; output: ${output}`));
    }, 30_000);
    const finish = (error, hostId) => {
      clearTimeout(timer);
      child.stdout?.off('data', onOutput);
      child.stderr?.off('data', onOutput);
      child.off('error', onError);
      child.off('exit', onExit);
      if (error) reject(error);
      else resolve(hostId);
    };
    const onOutput = (chunk) => {
      output = (output + chunk.toString()).slice(-16 * 1024);
      const match = output.match(/host id:\s*([^\s]+)/i);
      if (match) finish(undefined, match[1]);
    };
    const onError = (error) => finish(error);
    const onExit = (code, signal) => {
      finish(new Error(`locho host exited before startup: code=${code} signal=${signal}; output: ${output}`));
    };
    child.stdout?.on('data', onOutput);
    child.stderr?.on('data', onOutput);
    child.once('error', onError);
    child.once('exit', onExit);
  });
}

async function runCommand(binary, args, env) {
  return execFileAsync(binary, args, {
    env,
    encoding: 'utf8',
    maxBuffer: 1024 * 1024,
    timeout: 20_000,
  });
}

async function waitForRelay(binary, hostId, env) {
  let lastOutput = '';
  for (let attempt = 0; attempt < 12; attempt += 1) {
    try {
      const result = await runCommand(binary, ['diagnose', '--host-id', hostId], env);
      lastOutput = `${result.stdout}\n${result.stderr}`;
      console.log(`[N0 fixture] ${lastOutput.trim()}`);
      const match = lastOutput.match(/relay:\s+(https:\/\/[^)\s]+)/);
      if (!match) throw new Error('N0 diagnostics did not report a relay URL');
      const relay = new URL(match[1]);
      relay.hostname = relay.hostname.replace(/\.$/, '');
      return relay.toString();
    } catch (error) {
      lastOutput = `${error.stdout ?? ''}\n${error.stderr ?? ''}`;
      await sleep(5_000);
    }
  }
  throw new Error(`N0 relay connectivity did not become ready:\n${lastOutput}`);
}

async function stopHost(child) {
  if (!child.pid) return;
  if (child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolve) => {
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
    }, 10_000);
    child.once('exit', () => {
      clearTimeout(timer);
      resolve();
    });
    child.kill('SIGTERM');
  });
}

export default async function globalSetup() {
  if (process.env.LOCHO_N0_INTEGRATION !== '1') return undefined;

  const binary = process.env.LOCHO_HOST_BINARY;
  if (!binary) throw new Error('LOCHO_HOST_BINARY is required for N0 integration');
  const fixtureFile = process.env.LOCHO_N0_FIXTURE_FILE;
  if (!fixtureFile) throw new Error('LOCHO_N0_FIXTURE_FILE is required for N0 integration');

  const stateDir = await mkdtemp(join(tmpdir(), 'locho-web-n0-'));
  let host;
  let notesApp;
  const pending = new Map();
  const requests = new Map();
  const upstream = createServer((request, response) => {
    if (request.url === '/stream' || request.url === '/stream-idle') {
      response.writeHead(200, { 'Content-Type': 'text/event-stream' });
      response.write(': connected\n\n');
      const timer = request.url === '/stream' ? setInterval(() => response.write(': heartbeat\n\n'), 100) : undefined;
      pending.set(request.headers.host, response);
      response.on('close', () => { clearInterval(timer); pending.delete(request.headers.host); });
      return;
    }
    if (request.url === '/truncated') {
      response.writeHead(200, { 'Content-Length': 100 }); response.write('partial');
      setTimeout(() => response.destroy(), 100); return;
    }
    if (request.url === '/echo') {
      const chunks = [];
      request.on('data', (chunk) => chunks.push(chunk));
      request.on('end', () => {
        const body = Buffer.concat(chunks);
        response.writeHead(200, { 'Content-Length': body.length, 'X-Method': request.method, 'X-Authority': request.headers.host });
        response.end(body);
      });
      return;
    }
    if (request.url === '/chunked') {
      response.writeHead(200, { 'Content-Type': 'application/octet-stream', 'X-Test': ['one', 'two'] });
      response.write(Buffer.from([0, 255]));
      response.end(Buffer.from([1, 128]));
      return;
    }
    if (request.url === '/redirect') {
      response.writeHead(302, { Location: 'https://example.invalid/', 'Content-Length': 0 }); response.end(); return;
    }
    if (request.url === '/empty') {
      response.writeHead(204); response.end(); return;
    }
    if (request.url === '/stall') {
      response.on('close', () => {}); return;
    }
    if (request.method === 'GET' && request.url === '/health') {
      const authority = request.headers.host;
      requests.set(authority, (requests.get(authority) ?? 0) + 1);
      if (authority?.startsWith('stall-')) {
        pending.set(authority, response);
        response.on('close', () => pending.delete(authority));
        return;
      }
      response.writeHead(200, {
        Connection: 'close',
        'Content-Length': Buffer.byteLength(BODY),
        'Content-Type': 'text/plain',
      });
      response.end(BODY);
      return;
    }
    response.writeHead(404, { Connection: 'close' });
    response.end();
  });
  const control = createServer((request, response) => {
    const url = new URL(request.url, 'http://localhost');
    const authority = url.searchParams.get('authority');
    if (request.method === 'POST' && url.pathname === '/interrupt') {
      pending.get(authority)?.destroy();
    }
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ active: pending.has(authority), requests: requests.get(authority) ?? 0 }));
  });
  const cleanup = async () => {
    try {
      if (host) await stopHost(host);
    } finally {
      upstream.closeAllConnections();
      await new Promise((resolve) => upstream.close(resolve));
      control.closeAllConnections();
      await new Promise((resolve) => control.close(resolve));
      await notesApp?.close();
      await rm(fixtureFile, { force: true });
      await rm(stateDir, { recursive: true, force: true });
    }
  };
  try {
    const upstreamPort = await listen(upstream);
    const controlPort = await listen(control);
    let notesConfig = '';
    if (process.env.LOCHO_REFERENCE_API) {
      const { createApp, addUser } = await import(pathToFileURL(join(process.env.LOCHO_REFERENCE_API, 'apps/api/dist/src/app.js')).href);
      const notes = await createApp();
      notesApp = notes.app;
      await addUser(notes.db, 'browser@example.test', 'synthetic-browser-password');
      const address = await notesApp.listen({ host: '127.0.0.1', port: 0 });
      notesConfig = `\n[[services]]\nname = "notes"\ntype = "tcp"\nendpoint = "127.0.0.1:${new URL(address).port}"\n`;
    }
    const configPath = join(stateDir, 'locho.toml');
    await writeFile(
      configPath,
      `[[services]]\nname = "${SERVICE}"\ntype = "tcp"\nendpoint = "127.0.0.1:${upstreamPort}"\n\n[[services]]\nname = "unavailable"\ntype = "tcp"\nendpoint = "127.0.0.1:1"\n${notesConfig}`,
    );

    const env = { ...process.env, LOCHO_STATE_DIR: stateDir };
    host = spawn(binary, ['host', '--config', configPath], {
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const hostId = await waitForHost(host);
    // Continue draining output after readiness so a full pipe cannot stall the host.
    host.stdout.resume();
    host.stderr.resume();
    const share = await runCommand(binary, ['share', SERVICE, '--config', configPath], env);
    const match = share.stdout.match(new RegExp(`locho attach (\\S+) (${SERVICE}:tcp:\\S+)`));
    if (!match || match[1] !== hostId) {
      throw new Error('unexpected share output for N0 fixture');
    }
    const peerRelayUrl = await waitForRelay(binary, hostId, env);
    const ticket = await runCommand(binary, ['share', SERVICE, '--config', configPath, '--ticket', '--peer-relay-url', peerRelayUrl], env);
    const notesTicket = notesApp ? (await runCommand(binary, ['share', 'notes', '--config', configPath, '--ticket', '--peer-relay-url', peerRelayUrl], env)).stdout.trim() : undefined;
    const unavailable = await runCommand(binary, ['secret', 'unavailable', '--config', configPath], env);

    await writeFile(
      fixtureFile,
      JSON.stringify({
        capability: match[2],
        ticket: ticket.stdout.trim(),
        notesTicket,
        expectedBody: BODY.trim(),
        expectedPath: 'relay',
        hostId,
        peerRelayUrl,
        controlUrl: `http://127.0.0.1:${controlPort}`,
        unavailableCapability: unavailable.stdout.trim(),
      }),
      { encoding: 'utf8', mode: 0o600 },
    );

    return cleanup;
  } catch (error) {
    await cleanup();
    throw error;
  }
}
