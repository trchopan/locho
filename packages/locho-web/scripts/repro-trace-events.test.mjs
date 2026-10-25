import { test } from 'node:test';
import assert from 'node:assert/strict';
import { traceEvent } from './repro-trace-events.mjs';

test('frame events retain offsets and packet metadata without payload text', () => {
  const sent = traceEvent('TRACE endpoint{id=private-endpoint}:drive{id=2}:send{space=Data pn=19 path_id=0}: noq_proto::connection::packet_builder: frame=STREAM id: client bidirectional stream 0 off: 0 len: 61 fin: false secret=do-not-copy');
  assert.equal(sent.direction, 'send');
  assert.equal(sent.length, 61);
  assert.equal(sent.packetNumber, 19);
  assert.equal(sent.drive, 2);
  assert.ok(!JSON.stringify(sent).includes('private-endpoint'));
  assert.ok(!JSON.stringify(sent).includes('do-not-copy'));
  const recv = traceEvent('TRACE endpoint{id=other}:drive{id=0}:recv{space=Data pn=19}: got frame STREAM id=client bidirectional stream 0 offset=0 len=61 fin=false');
  assert.equal(recv.direction, 'receive');
  assert.equal(recv.offset, 0);
  assert.equal(recv.length, 61);
});

test('only known milestone names and fixed transport events are retained', () => {
  assert.equal(traceEvent('arbitrary private raw log'), undefined);
  assert.equal(traceEvent('stage=capability secret=private'), undefined);
  assert.deepEqual(traceEvent('DEBUG stage="connect_return" online=false endpoint=private'), { kind: 'milestone', stage: 'connect_return', online: false });
  assert.deepEqual(traceEvent('TRACE frame=ACK ranges=1..9'), { kind: 'ack_frame' });
  assert.deepEqual(traceEvent('TRACE abandoning path'), { kind: 'path_abandoned' });
  assert.deepEqual(traceEvent('2026-10-24T10:20:30.123456Z DEBUG stage="authorization_reply_written"'),
    { kind: 'milestone', stage: 'authorization_reply_written', atUnixMs: Date.parse('2026-10-24T10:20:30.123Z') });
});

test('ACK ranges preserve exclusive bounds, path and packet number space', () => {
  const event = traceEvent('TRACE drive{id=7}:recv{space=Data pn=23}: got frame PATH_ACK path_id: 2 ranges: [0..7,9..11] delay: 0µs');
  assert.deepEqual(event.ackRanges, [{ start: 0, endExclusive: 7 }, { start: 9, endExclusive: 11 }]);
  assert.equal(event.ackPathId, 2);
  assert.equal(event.packetDirection, 'recv');
  assert.equal(event.packetSpace, 'Data');
  assert.equal(event.packetNumber, 23);
  assert.equal(traceEvent('TRACE got frame ACK ranges: [0..7] delay: 0µs largest: 6').ackPathId, 0);
});
