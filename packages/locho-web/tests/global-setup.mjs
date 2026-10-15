import { createServer } from 'node:http';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

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
      reject(new Error(`timed out waiting for locho host; output: ${output}`));
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
      output += chunk.toString();
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
      return new URL(match[1]).toString();
    } catch (error) {
      lastOutput = `${error.stdout ?? ''}\n${error.stderr ?? ''}`;
      await sleep(5_000);
    }
  }
  throw new Error(`N0 relay connectivity did not become ready:\n${lastOutput}`);
}

async function stopHost(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  await new Promise((resolve) => {
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      resolve();
    }, 10_000);
    child.once('exit', () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

export default async function globalSetup() {
  if (process.env.LOCHO_N0_INTEGRATION !== '1') return undefined;

  const binary = process.env.LOCHO_HOST_BINARY;
  if (!binary) throw new Error('LOCHO_HOST_BINARY is required for N0 integration');
  const fixtureFile = process.env.LOCHO_N0_FIXTURE_FILE;
  if (!fixtureFile) throw new Error('LOCHO_N0_FIXTURE_FILE is required for N0 integration');

  const stateDir = await mkdtemp(join(tmpdir(), 'locho-web-n0-'));
  const upstream = createServer((request, response) => {
    if (request.method === 'GET' && request.url === '/health') {
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
  const upstreamPort = await listen(upstream);
  const configPath = join(stateDir, 'locho.toml');
  await writeFile(
    configPath,
    `[[services]]\nname = "${SERVICE}"\ntype = "tcp"\nendpoint = "127.0.0.1:${upstreamPort}"\n`,
  );

  const env = { ...process.env, LOCHO_STATE_DIR: stateDir };
  const host = spawn(binary, ['host', '--config', configPath], {
    env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  try {
    const hostId = await waitForHost(host);
    const share = await runCommand(binary, ['share', SERVICE, '--config', configPath], env);
    const match = share.stdout.match(new RegExp(`locho attach (\\S+) (${SERVICE}:tcp:\\S+)`));
    if (!match || match[1] !== hostId) {
      throw new Error('unexpected share output for N0 fixture');
    }
    const peerRelayUrl = await waitForRelay(binary, hostId, env);

    await writeFile(
      fixtureFile,
      JSON.stringify({
        capability: match[2],
        expectedBody: BODY.trim(),
        expectedPath: 'relay',
        hostId,
        peerRelayUrl,
      }),
      { encoding: 'utf8', mode: 0o600 },
    );

    return async () => {
      await stopHost(host);
      await new Promise((resolve) => upstream.close(resolve));
      await rm(fixtureFile, { force: true });
      await rm(stateDir, { recursive: true, force: true });
    };
  } catch (error) {
    await stopHost(host);
    await new Promise((resolve) => upstream.close(resolve));
    await rm(fixtureFile, { force: true });
    await rm(stateDir, { recursive: true, force: true });
    throw error;
  }
}
