// Allowlisted transport metadata only. Never copy raw log text, frame payloads,
// endpoint IDs, addresses, URLs, application headers or error messages.
import { sha256 } from './repro-provenance.mjs';

const stages = new Set(['authorization_request_start', 'authorization_request_written', 'authorization_reply_decoded',
  'head_prefix_read', 'head_bytes_read', 'upstream_connected', 'authorization_reply_start', 'authorization_reply_written', 'connect_return']);
const stream = '(client|server) (bi|uni)directional stream (\\d+)';
const sentStream = new RegExp(`STREAM id: ${stream} off: (\\d+) len: (\\d+) fin: (true|false)`);
const receivedStream = new RegExp(`got frame STREAM.*\\bid=${stream} offset=(\\d+) len=(\\d+) fin=(true|false)`);

export function traceEvent(raw) {
  const line = raw.replace(/\x1b\[[0-9;]*m/g, '');
  let event;
  const stage = line.match(/\bstage="?([a-z_]+)/)?.[1];
  if (stages.has(stage)) {
    event = { kind: 'milestone', stage };
    const online = line.match(/\bonline=(true|false)/)?.[1];
    if (online) event.online = online === 'true';
  } else {
    const sent = line.match(sentStream);
    const received = line.match(receivedStream);
    const frame = sent ?? received;
    if (frame) event = { kind: 'stream_frame', direction: sent ? 'send' : 'receive',
      initiator: frame[1], streamType: frame[2], streamIndex: Number(frame[3]),
      offset: Number(frame[4]), length: Number(frame[5]), fin: frame[6] === 'true' };
    else if (/frame=(PATH_)?ACK(?:_ECN)?\b/.test(line) || /got frame (PATH_)?ACK(?:_ECN)?\b/.test(line)) {
      event = { kind: 'ack_frame' };
      const ranges = line.match(/\branges:\s*\[([\d.,\s]*)\]/)?.[1];
      if (ranges !== undefined) event.ackRanges = [...ranges.matchAll(/(\d+)\.\.(\d+)/g)]
        .slice(0, 512).map((match) => ({ start: Number(match[1]), endExclusive: Number(match[2]) }));
      const ackPath = line.match(/\bPATH_ACK(?:_ECN)? path_id: (\d+)/)?.[1];
      if (ackPath) event.ackPathId = Number(ackPath);
      else if (/\bACK(?:_ECN)? ranges:/.test(line)) event.ackPathId = 0;
    }
    else if (/\bdeclared lost\b|\bpackets lost\b|\bpacket lost\b/.test(line)) event = { kind: 'loss' };
    else if (/\babandoning path\b/.test(line)) event = { kind: 'path_abandoned' };
    else if (/\bnew path validated\b/.test(line)) event = { kind: 'path_validated' };
  }
  if (!event) return undefined;
  const timestamp = line.match(/^\s*(\d{4}-\d{2}-\d{2}T[\d:.]+Z)\b/)?.[1];
  if (timestamp && Number.isFinite(Date.parse(timestamp))) event.atUnixMs = Date.parse(timestamp);
  // Pseudonyms are useful within one disposable capture, not public identity
  // claims. Different span forms may be absent; do not infer identity by omission.
  const endpoint = line.match(/\bendpoint\{id=([^\s}]+)/)?.[1];
  if (endpoint) event.endpoint = sha256(endpoint).slice(0, 12);
  const drive = line.match(/\bdrive\{id=(\d+)/)?.[1];
  if (drive) event.drive = Number(drive);
  const packet = line.match(/\b(send|recv)\{[^}]*\bpn=(\d+)/);
  if (packet) { event.packetDirection = packet[1]; event.packetNumber = Number(packet[2]); }
  const space = line.match(/\b(?:send|recv)\{[^}]*\bspace=(Initial|Handshake|Data)\b/)?.[1];
  if (space) event.packetSpace = space;
  const path = line.match(/\bpath_id=(\d+)/)?.[1];
  if (path) event.pathId = Number(path);
  const client = line.match(/\bclient_id=([0-9a-f]+)/)?.[1];
  if (client) event.client = sha256(client).slice(0, 12);
  return event;
}
