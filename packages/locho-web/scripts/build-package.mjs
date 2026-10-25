import { cp, mkdir, rm, readFile, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { assertReleaseBindings } from './release-guards.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const out = new URL('../dist/package/', import.meta.url);
const bindings = await readFile(new URL('../src/generated/locho_web.js', import.meta.url), 'utf8');
assertReleaseBindings(bindings);
await rm(out, { recursive: true, force: true });
await mkdir(out, { recursive: true });
const result = spawnSync('npx', ['--no-install', 'tsc', '-p', 'tsconfig.package.json'], { cwd: root, stdio: 'inherit' });
if (result.status !== 0) process.exit(result.status ?? 1);
await mkdir(new URL('generated/', out));
for (const name of ['locho_web.js', 'locho_web.d.ts', 'locho_web_bg.wasm', 'locho_web_bg.wasm.d.ts']) {
  await cp(new URL(`../src/generated/${name}`, import.meta.url), new URL(`generated/${name}`, out));
}
await cp(new URL('../../../LICENSE', import.meta.url), new URL('../LICENSE', import.meta.url));
await cp(new URL('../../../docs/browser-http.md', import.meta.url), new URL('HTTP.md', out));
await cp(new URL('../../../docs/tickets.md', import.meta.url), new URL('TICKETS.md', out));
const { version } = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'));
const revision = spawnSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' });
if (revision.status !== 0) throw new Error('Package build requires source revision');
// wasm-bindgen replaces the checked-in declaration stub during every build.
const status = spawnSync('git', ['status', '--porcelain', '--untracked-files=normal', '--', '.', ':(exclude)packages/locho-web/src/generated/**'], { cwd: resolve(root, '../..'), encoding: 'utf8' });
if (status.status !== 0) throw new Error('Could not inspect package source status');
await writeFile(new URL('build.json', out), JSON.stringify({ version, revision: revision.stdout.trim(), dirty: Boolean(status.stdout.trim()), wireProtocol: 3, ticketFormat: 1 }, null, 2) + '\n');
