import { test } from 'node:test';
import assert from 'node:assert/strict';
import { hostTraceFilter } from './repro-trace-filter.mjs';

test('all diagnostic host binaries have matching Rust target filters', () => {
  for (const mode of ['milestones', 'frames', 'transport']) {
    const directives = hostTraceFilter(mode).split(',');
    for (const target of ['locho', 'locho_browser_repro', 'n0_trace', 'locho_core']) {
      assert.ok(directives.includes(`${target}=debug`), `${mode} must cover ${target}`);
    }
  }
  assert.equal(hostTraceFilter('none'), 'off');
  assert.throws(() => hostTraceFilter('invalid'));
});
