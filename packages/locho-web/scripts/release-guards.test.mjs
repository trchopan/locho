import { test } from 'node:test';
import assert from 'node:assert/strict';
import { assertReleaseBindings } from './release-guards.mjs';

test('ordinary generated SDK bindings pass the release guard', () => {
  assert.doesNotThrow(() => assertReleaseBindings('export function connect() {}\nexport class WebClient {}'));
});

for (const name of ['connect_repro', 'connect_n0_repro', 'configure_repro_client_policy', 'initialize_repro_tracing', 'repro_stats']) {
  test(`investigation export ${name} cannot enter a release package`, () => {
    assert.throws(() => assertReleaseBindings(`export function ${name}() {}`), /must never be published/);
  });
}
