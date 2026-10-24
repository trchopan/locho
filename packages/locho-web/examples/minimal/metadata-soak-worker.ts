import { MetadataSoak } from './metadata-soak';

interface SoakEvent {
  data: { action: 'open' | 'batch' | 'close'; ticket?: string; authority?: string };
  ports: MessagePort[];
  waitUntil(promise: Promise<void>): void;
}
let owner: MetadataSoak | undefined;
globalThis.addEventListener('message', (event) => {
  const { data, ports, waitUntil } = event as unknown as SoakEvent;
  const run = async () => {
    try {
      if (data.action === 'open') owner = await MetadataSoak.open(data.ticket!, data.authority!);
      else if (data.action === 'batch') {
        if (!owner) throw new Error('missingOwner');
        ports[0].postMessage({ result: await owner.batch() }); return;
      } else { await owner?.close(); owner = undefined; }
      ports[0].postMessage({ result: true });
    } catch { ports[0].postMessage({ failed: true }); }
  };
  // Keep the worker alive for each bounded batch, without exporting session state.
  waitUntil.call(event, run());
});
