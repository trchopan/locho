import { spawnSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { build } from 'vite';
import { treeHashes, wasmSourceHashes, sameHashes } from './repro-provenance.mjs';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const pkg = resolve(root, 'packages/locho-web');
const target = resolve(root, 'target/browser-repro-wasm');
const generated = resolve(pkg, 'dist/repro/generated');
function run(command, args) {
  const result = spawnSync(command, args, { cwd: root, stdio: 'inherit', env: { ...process.env, CARGO_TARGET_DIR: target } });
  if (result.status !== 0) throw new Error(`${command} failed`);
}
await mkdir(generated, { recursive: true });
const sourceHashes = await wasmSourceHashes(root);
run('cargo', ['build', '--locked', '--release', '-p', 'locho-web', '--target', 'wasm32-unknown-unknown', '--features', 'browser-repro']);
run('wasm-bindgen', [resolve(target, 'wasm32-unknown-unknown/release/locho_web.wasm'), '--target', 'web', '--out-dir', generated]);
await build({ configFile: false, root: resolve(pkg, 'tests/repro-page'),
  plugins: [{ name: 'same-reproduction-wasm', enforce: 'pre', resolveId(source, importer) {
    if (source === './generated/locho_web.js' && importer === resolve(pkg, 'src/index.ts')) {
      return resolve(generated, 'locho_web.js');
    }
  } }],
  build: { outDir: resolve(pkg, 'dist/repro/browser'), emptyOutDir: true },
});
if (!sameHashes(sourceHashes, await wasmSourceHashes(root))) throw new Error('Source changed during reproduction build');
await writeFile(resolve(pkg, 'dist/repro/build.json'), JSON.stringify({
  schema: 1, feature: 'browser-repro', profile: 'release', target: 'wasm32-unknown-unknown',
  frameTrace: 'noq_proto::connection=trace', sourceHashes,
  generated: await treeHashes(generated), browser: await treeHashes(resolve(pkg, 'dist/repro/browser')),
}, null, 2) + '\n');
