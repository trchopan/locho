import { test } from 'node:test';
import assert from 'node:assert/strict';
import { installRunner } from '../tests/repro-page/runner.js';

test('runner performs one health call and closes each independent client', async () => {
  const calls = [];
  let id = 0;
  installRunner({ initializeLocho: async () => {}, LochoClient: {
    connect: async () => {
      const client = id++;
      return { status: { path: 'relay' },
        health: async (_capability, authority) => {
          calls.push({ client, stage: 'health', authority });
          return new Response('locho-repro-health\n');
        },
        close: async () => { calls.push({ client, stage: 'close' }); },
      };
    },
  } });
  const results = await globalThis.lochoRepro({ mode: 'n0', trace: 'none', cycle: 7 });
  assert.equal(results.length, 2);
  for (const result of results) {
    assert.equal(result.status, 200);
    assert.equal(result.correctBody, true);
    assert.equal(calls.filter((call) => call.client === result.index && call.stage === 'health').length, 1);
    assert.equal(calls.filter((call) => call.client === result.index && call.stage === 'close').length, 1);
    assert.equal(result.timeline.at(-1).stage, 'closed');
  }
});

test('authorization failure is not replayed and error messages are not retained', async () => {
  let healthCalls = 0, closes = 0;
  installRunner({ initializeLocho: async () => {}, LochoClient: {
    connect: async () => ({ status: { path: 'relay', peer: 'connected' },
      health: async () => { healthCalls++; throw Object.assign(new Error('private capability must not appear'), { code: 'timeout' }); },
      close: async () => { closes++; },
    }),
  } });
  const results = await globalThis.lochoRepro({ mode: 'n0', trace: 'none', cycle: 0 });
  assert.equal(healthCalls, 2);
  assert.equal(closes, 2);
  assert.ok(results.every((result) => result.errorCode === 'timeout' && result.stage === 'health'));
  assert.ok(!JSON.stringify(results).includes('private capability'));
});

test('counter snapshots occur only after failure, before that client closes', async () => {
  let id = 0;
  const calls = [];
  installRunner({ initializeLocho: async () => {}, LochoClient: {
    connect: async () => {
      const index = id++;
      return { index, status: { path: 'relay' },
        health: async () => {
          if (index === 0) return new Response('locho-repro-health\n');
          calls.push('failure');
          throw { code: 'timeout' };
        },
        close: async () => { calls.push(`close-${index}`); },
      };
    },
  }, diagnosticStats: (client) => { calls.push(`stats-${client.index}`); return { streamFramesRx: 0 }; } });
  const results = await globalThis.lochoRepro({ mode: 'n0', trace: 'none', cycle: 0 });
  assert.ok(!calls.includes('stats-0'));
  assert.ok(calls.indexOf('stats-1') > calls.indexOf('failure'));
  assert.ok(calls.indexOf('close-1') > calls.indexOf('stats-1'));
  assert.deepEqual(results[1].transportStats, { streamFramesRx: 0 });
});

test('host snapshot callback completes before a failed client is closed', async () => {
  let closes = 0, snapshots = 0;
  globalThis.__lochoReproFailure = async () => {
    assert.equal(closes, 0);
    snapshots++;
    return [{ streamFramesTx: 1 }];
  };
  try {
    installRunner({ initializeLocho: async () => {}, LochoClient: {
      connect: async () => ({ status: { path: 'relay' },
        health: async () => { throw { code: 'timeout' }; },
        close: async () => { closes++; },
      }),
    } });
    const results = await globalThis.lochoRepro({ mode: 'n0', trace: 'none', cycle: 0 });
    assert.equal(snapshots, 2);
    assert.equal(closes, 2);
    assert.ok(results.every((result) => result.hostTransportStats[0].streamFramesTx === 1));
  } finally { delete globalThis.__lochoReproFailure; }
});

test('both policy controls use the same public JS connection path', async () => {
  for (const canonical of [false, true]) {
    const calls = [];
    installRunner({ initializeLocho: async () => { calls.push('initialize'); },
      configurePolicy: (value) => { calls.push(`policy-${value}`); },
      LochoClient: { connect: async () => {
        calls.push('public-connect');
        return { status: { path: 'relay' }, health: async () => new Response('locho-repro-health\n'), close: async () => {} };
      } },
    });
    const results = await globalThis.lochoRepro({ mode: 'n0', trace: 'none', canonicalN0Home: canonical, cycle: 0 });
    assert.deepEqual(calls, ['initialize', `policy-${canonical}`, 'public-connect', 'public-connect']);
    assert.ok(results.every((result) => result.status === 200));
  }
});
