import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFile, readdir } from 'node:fs/promises';
import { resolve, join, relative } from 'node:path';

export const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

export async function fileHashes(base, paths) {
  const entries = await Promise.all([...new Set(paths)].sort().map(async (path) => [path, sha256(await readFile(resolve(base, path)))]));
  return Object.fromEntries(entries);
}

export async function treeHashes(base) {
  const paths = [];
  async function visit(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await visit(path);
      else if (entry.isFile()) paths.push(relative(base, path));
      else throw new Error('unexpectedArtifactType');
    }
  }
  await visit(base);
  return fileHashes(base, paths);
}

export async function wasmSourceHashes(root) {
  const paths = execFileSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard', '--',
    'Cargo.toml', 'Cargo.lock', 'crates/locho-core', 'crates/locho-web', 'packages/locho-web/src',
    'packages/locho-web/tests/repro-page', 'packages/locho-web/scripts/build-repro.mjs',
    'packages/locho-web/scripts/repro-provenance.mjs'], { cwd: root, encoding: 'utf8' }).split('\0').filter(Boolean);
  return fileHashes(root, paths);
}

export function sameHashes(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}
