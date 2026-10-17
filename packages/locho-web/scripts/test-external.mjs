import { mkdtemp, readFile, writeFile, mkdir, rm, cp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root = fileURLToPath(new URL('../', import.meta.url));
const temporary = await mkdtemp(join(tmpdir(), 'locho-package-consumer-'));
function run(command, args, cwd, env = process.env, capture = false) {
  const result = spawnSync(command, args, { cwd, env, encoding: 'utf8', stdio: capture ? 'pipe' : 'inherit' });
  if (result.status !== 0) throw new Error(`${command} failed (${result.status}): ${capture ? result.stderr : ''}`);
  return result.stdout;
}
try {
  const pack = JSON.parse(run('npm', ['pack', '--json', '--pack-destination', temporary], root, process.env, true))[0];
  const files = pack.files.map((file) => file.path);
  const expected = [
    'package.json', 'LICENSE', 'README.md', 'CHANGELOG.md', 'dist/package/index.js', 'dist/package/index.d.ts',
    'dist/package/generated/locho_web.js', 'dist/package/generated/locho_web.d.ts',
    'dist/package/generated/locho_web_bg.wasm', 'dist/package/generated/locho_web_bg.wasm.d.ts',
    'dist/package/HTTP.md', 'dist/package/TICKETS.md', 'dist/package/build.json',
  ];
  if (files.length !== expected.length || expected.some((name) => !files.includes(name))) {
    throw new Error(`Unexpected tarball contents: ${files.join(', ')}`);
  }
  const declaration = await readFile(join(root, 'dist/package/index.d.ts'), 'utf8');
  if (/generated\/|WebCancellation|connectRaw|healthRaw|requestRaw/.test(declaration)) throw new Error('Raw bindings leaked in public declarations');
  const artifactDir = join(root, 'dist/artifacts');
  await mkdir(artifactDir, { recursive: true });
  await cp(join(temporary, pack.filename), join(artifactDir, pack.filename));
  await writeFile(join(artifactDir, 'pack-manifest.json'), JSON.stringify(pack, null, 2));

  const consumer = join(temporary, 'consumer');
  await mkdir(consumer);
  await writeFile(join(consumer, 'package.json'), JSON.stringify({ private: true, type: 'module', dependencies: {
    locho_web: process.env.LOCHO_NPM_VERSION ?? `file:../${pack.filename}`, vite: '7.3.7', typescript: '5.9.3',
  } }));
  run('npm', ['install', '--ignore-scripts', '--no-audit', '--no-fund'], consumer);
  for (const name of ['index.html', 'main.ts', 'lifecycle.html', 'lifecycle.ts']) {
    const source = await readFile(join(root, 'examples/minimal', name), 'utf8');
    await writeFile(join(consumer, name), source.replaceAll("'../../src/index'", "'locho_web'"));
  }
  await writeFile(join(consumer, 'types.ts'), `import { LochoClient, LochoTicket, initializeLocho, type LochoRequestOptions, type LochoStreamRequestOptions } from 'locho_web';
const options: LochoRequestOptions = { method: 'POST', body: new Uint8Array([1]) };
const streamOptions: LochoStreamRequestOptions = { setupTimeoutMs: 15000, idleTimeoutMs: 75000 };
async function stream(client: LochoClient) { const response: Response = await client.fetchStream('/api/events', streamOptions); await response.body?.cancel(); }
async function consume(text: string) { await initializeLocho(); const ticket = await LochoTicket.parse(text); const client = await LochoClient.connect({ ticket }); try { return await client.fetch('/api/notes', options); } finally { await client.close(); } }
void consume;
void stream;
`);
  await writeFile(join(consumer, 'initialize.html'), '<!doctype html><p id="result">initializing</p><script type="module" src="./initialize.ts"></script>');
  await writeFile(join(consumer, 'initialize.ts'), `import { initializeLocho, LochoTicket } from 'locho_web';
import wasmUrl from 'locho_web/wasm?url';
await Promise.all([initializeLocho(wasmUrl), initializeLocho()]);
const code = await LochoTicket.parse('invalid').catch((error) => error.code);
document.querySelector('#result').textContent = code === 'invalidTicket' ? 'initialized' : 'failed';
`);
  await writeFile(join(consumer, 'tsconfig.json'), JSON.stringify({ compilerOptions: { target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler', strict: true, noEmit: true, skipLibCheck: false, lib: ['ES2022', 'DOM', 'DOM.Iterable'] }, include: ['types.ts'] }));
  run('npx', ['--no-install', 'tsc'], consumer);
  await writeFile(join(consumer, 'vite.config.js'), `export default { base: '/sdk/', build: { rollupOptions: { input: { main: 'index.html', lifecycle: 'lifecycle.html', initialize: 'initialize.html' } } } };`);
  run('npx', ['--no-install', 'vite', 'build'], consumer);
  const server = join(temporary, 'server');
  await mkdir(server);
  await cp(join(consumer, 'dist'), join(server, 'sdk'), { recursive: true });
  // Existing tests exercise only the installed tarball via these external entry points.
  run('npx', ['--no-install', 'playwright', 'test'], root, {
    ...process.env,
    LOCHO_BROWSER_URL: 'http://127.0.0.1:4173/sdk/',
    LOCHO_BROWSER_DIST: resolve(server),
  });
} finally {
  await rm(temporary, { recursive: true, force: true });
}
