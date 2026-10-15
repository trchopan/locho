import { mkdir, rm } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { resolve } from 'node:path';

const root = resolve(import.meta.dirname, '../../..');
const out = resolve(import.meta.dirname, '../src/generated');
await rm(out, { recursive: true, force: true });
await mkdir(out, { recursive: true });
function run(command, args, cwd) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, { cwd, stdio: 'inherit' });
    child.once('error', reject);
    child.once('exit', (code) => code === 0 ? resolvePromise() : reject(new Error(`${command} exited ${code}`)));
  });
}
await run('cargo', ['build', '--locked', '--release', '-p', 'locho-web', '--target', 'wasm32-unknown-unknown'], root);
await run('wasm-bindgen', [resolve(root, 'target/wasm32-unknown-unknown/release/locho_web.wasm'), '--target', 'web', '--out-dir', out], root);
