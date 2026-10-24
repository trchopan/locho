// Service-worker integration proof. Only non-sensitive assertions cross back to the page.
import { LochoClient, LochoTicket } from '../../src/index';

interface ProofEvent {
  data: { ticket: string; streaming: boolean };
  ports: MessagePort[];
  waitUntil(promise: Promise<void>): void;
}

async function prove({ data, ports }: ProofEvent) {
  const client = await LochoClient.connect({ ticket: await LochoTicket.parse(data.ticket) });
  try {
    const request = data.streaming ? client.fetchStreamWithMetadata.bind(client) : client.fetchWithMetadata.bind(client);
    const login = await request('/cookie-login', { method: 'POST' });
    const cookies = login.upstreamHeaders.filter(([name]) => name === 'set-cookie');
    await login.response.body?.cancel();
    // This fixture deliberately has a single session cookie, not a general cookie jar.
    let session = cookies.find(([, value]) => value.startsWith('session='))?.[1].split(';')[0];
    const authenticated = await client.fetch('/cookie-session', { headers: { Cookie: session! } });
    await authenticated.body?.cancel();
    const logout = await request('/cookie-logout', { method: 'POST' });
    const deleted = logout.upstreamHeaders.some(([name, value]) => name === 'set-cookie' && value.includes('Max-Age=0'));
    if (deleted) session = undefined;
    const denied = await client.fetch('/cookie-session', { headers: session ? { Cookie: session } : {} });
    await denied.body?.cancel();
    ports[0].postMessage({ cookies: cookies.length, expiryPreserved: cookies[1]?.[1].includes('Expires=Wed, 21 Oct 2037'), authenticated: authenticated.status, deleted, denied: denied.status });
  } finally { await client.close(); }
}

globalThis.addEventListener('message', (event) => {
  const proof = event as unknown as ProofEvent;
  proof.waitUntil(prove(proof).catch(() => { proof.ports[0].postMessage({ failed: true }); }));
});
