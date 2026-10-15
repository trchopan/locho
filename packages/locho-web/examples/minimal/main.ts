import { LochoClient } from '../../src/index';

const output = document.querySelector<HTMLPreElement>('#output')!;
const host = document.querySelector<HTMLInputElement>('#host')!;
const peerRelay = document.querySelector<HTMLInputElement>('#peerRelay')!;
const capability = document.querySelector<HTMLInputElement>('#capability')!;
let current: AbortController | undefined;
window.addEventListener('pagehide', () => { current?.abort(); });
document.querySelector<HTMLButtonElement>('#connect')!.addEventListener('click', async () => {
  current?.abort();
  const controller = new AbortController();
  current = controller;
  output.textContent = 'connecting';
  let client: LochoClient | undefined;
  try {
    client = await LochoClient.connect({
      hostId: host.value.trim(),
      peerRelayUrl: peerRelay.value.trim() || undefined,
      signal: controller.signal,
    });
    const closeOnAbort = () => { void client?.close().catch(() => undefined); };
    controller.signal.addEventListener('abort', closeOnAbort, { once: true });
    try {
      const response = await client.health(capability.value, 'sample-api', controller.signal);
      const body = await response.text();
      if (current === controller && !controller.signal.aborted) {
        output.textContent = `peer=${client.status.state}/${client.status.path}\napi=${response.status}\n${body}`;
      }
    } finally {
      controller.signal.removeEventListener('abort', closeOnAbort);
    }
  } catch (error) {
    if (current === controller && !controller.signal.aborted) {
      output.textContent = error instanceof Error ? error.toString() : 'connection failed';
    }
  } finally {
    await client?.close().catch(() => undefined);
  }
});
