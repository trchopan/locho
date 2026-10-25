import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { sha256, treeHashes, sameHashes } from './repro-provenance.mjs';

test('artifact hashes detect file changes, additions and removals', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'locho-provenance-test-'));
  try {
    await mkdir(join(directory, 'assets'));
    await writeFile(join(directory, 'assets/module.wasm'), 'original');
    const original = await treeHashes(directory);
    assert.equal(original['assets/module.wasm'], sha256('original'));
    assert.ok(sameHashes(original, await treeHashes(directory)));
    await writeFile(join(directory, 'assets/module.wasm'), 'changed');
    assert.ok(!sameHashes(original, await treeHashes(directory)));
    await writeFile(join(directory, 'assets/module.wasm'), 'original');
    await writeFile(join(directory, 'extra.js'), 'extra');
    assert.ok(!sameHashes(original, await treeHashes(directory)));
    await rm(join(directory, 'extra.js'));
    assert.ok(sameHashes(original, await treeHashes(directory)));
    await rm(join(directory, 'assets/module.wasm'));
    assert.ok(!sameHashes(original, await treeHashes(directory)));
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('artifact trees reject symbolic links instead of hashing outside the build', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'locho-provenance-test-'));
  try {
    await writeFile(join(directory, 'real.js'), 'source');
    await symlink('real.js', join(directory, 'link.js'));
    await assert.rejects(treeHashes(directory), /unexpectedArtifactType/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
