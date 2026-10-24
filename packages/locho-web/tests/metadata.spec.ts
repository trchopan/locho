import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

test.describe('upstream response metadata over N0', () => {
  test.skip(process.env.LOCHO_N0_INTEGRATION !== '1', 'N0 fixture disabled');
  test.beforeEach(async ({ page }) => {
    await page.goto('./lifecycle.html');
    await page.waitForFunction(() => Boolean((window as any).lochoProof));
  });

  for (const streaming of [false, true]) {
    test(`${streaming ? 'streaming' : 'buffered'} cookie headers and application-managed session`, async ({ page }) => {
      const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
      const result = await page.evaluate(async ({ values, streaming }) => {
        const { LochoClient, LochoTicket } = (window as any).lochoProof;
        const client = await LochoClient.connect({ ticket: await LochoTicket.parse(values.ticket) });
        try {
          const request = streaming ? client.fetchStreamWithMetadata.bind(client) : client.fetchWithMetadata.bind(client);
          const login = await request('/cookie-login', { method: 'POST' });
          const cookies = login.upstreamHeaders.filter(([name]: [string, string]) => name === 'set-cookie');
          const repeated = login.upstreamHeaders.filter(([name]: [string, string]) => name === 'x-test');
          const frozen = Object.isFrozen(login) && Object.isFrozen(login.upstreamHeaders) && login.upstreamHeaders.every(Object.isFrozen);
          const body = await login.response.text();
          const headers = { Cookie: cookies[0][1].split(';')[0] };
          const authenticated = await client.fetch('/cookie-session', { headers });
          await authenticated.body?.cancel();
          const automatic = await client.fetch('/cookie-session');
          await automatic.body?.cancel();
          const logout = await request('/cookie-logout', { method: 'POST', headers });
          const deletion = logout.upstreamHeaders.filter(([name]: [string, string]) => name === 'set-cookie');
          // The app clears the narrowly scoped session when receiving deletion metadata.
          const denied = await client.fetch('/cookie-session');
          await denied.body?.cancel();
          const head = await request('/cookie-login', { method: 'HEAD' });
          const legacy = await client.fetch('/health');
          const legacyStream = await client.fetchStream('/health');
          await legacy.body?.cancel(); await legacyStream.body?.cancel();
          return { cookies, repeated, frozen, body, authenticated: authenticated.status, automatic: automatic.status, deletion, logout: [logout.response.status, logout.response.body === null], denied: denied.status, head: [head.response.body === null, head.upstreamHeaders.filter(([name]: [string, string]) => name === 'set-cookie').length], legacy: legacy instanceof Response && legacyStream instanceof Response };
        } finally { await client.close(); }
      }, { values, streaming });
      expect(result).toEqual({
        cookies: [['set-cookie', 'session=synthetic; Path=/; HttpOnly; Secure; SameSite=Lax'], ['set-cookie', 'preference=one; Expires=Wed, 21 Oct 2037 07:28:00 GMT; Path=/']],
        repeated: [['x-test', 'one'], ['x-test', 'two']], frozen: true, body: 'logged in', authenticated: 200, automatic: 401,
        deletion: [['set-cookie', 'session=; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/; HttpOnly']], logout: [204, true], denied: 401, head: [true, 2], legacy: true,
      });
    });

    for (const workerType of ['classic', 'module'] as const) {
      test(`${streaming ? 'streaming' : 'buffered'} metadata in a ${workerType} service worker`, async ({ page }) => {
        const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
        const result = await page.evaluate(async ({ values, streaming, workerType }) => {
          const { metadataWorkerUrl } = (window as any).lochoProof;
          const registration = await navigator.serviceWorker.register(metadataWorkerUrl, { type: workerType });
          try {
            const worker = registration.installing ?? registration.waiting ?? registration.active!;
            if (worker.state !== 'activated') await new Promise<void>((resolve, reject) => {
              worker.addEventListener('statechange', () => {
                if (worker.state === 'activated') resolve();
                if (worker.state === 'redundant') reject(new Error('Service worker installation failed'));
              });
            });
            return await new Promise((resolve, reject) => {
              const channel = new MessageChannel();
              const timer = setTimeout(() => { channel.port1.close(); reject(new Error('Service worker proof timed out')); }, 20_000);
              channel.port1.onmessage = ({ data }) => { clearTimeout(timer); channel.port1.close(); resolve(data); };
              worker.postMessage({ ticket: values.ticket, streaming }, [channel.port2]);
            });
          } finally { await registration.unregister(); }
        }, { values, streaming, workerType });
        expect(result).toEqual({ cookies: 2, expiryPreserved: true, authenticated: 200, deleted: true, denied: 401 });
      });
    }
  }

  test('streaming metadata precedes body completion and retains cancellation/close ownership', async ({ page }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const result = await page.evaluate(async (values) => {
      const { LochoClient, LochoTicket } = (window as any).lochoProof;
      const client = await LochoClient.connect({ ticket: await LochoTicket.parse(values.ticket) });
      const code = (promise: Promise<any>) => promise.then(() => 'success', (e) => e.code);
      try {
        const abort = new AbortController();
        const { response, upstreamHeaders } = await client.fetchStreamWithMetadata('/stream-idle', { signal: abort.signal });
        const reader = response.body.getReader(); await reader.read();
        const pending = code(reader.read()); abort.abort();
        const aborted = await pending;
        const cancel = await client.fetchStreamWithMetadata('/stream-idle');
        const c = cancel.response.body.getReader(); await c.read();
        const cancelling = c.read(); await c.cancel(); await cancelling;
        const setup = await code(client.fetchStreamWithMetadata('/stall', { setupTimeoutMs: 100 }));
        const idle = await client.fetchStreamWithMetadata('/stream-idle', { idleTimeoutMs: 100 });
        const i = idle.response.body.getReader(); await i.read();
        const idleCode = await code(i.read());
        const bodies = await Promise.all(Array.from({ length: 16 }, () => client.fetchStreamWithMetadata('/stream-idle')));
        const excess = await code(client.fetchWithMetadata('/health'));
        await Promise.all(bodies.map(({ response }) => response.body.cancel()));
        const health = await client.fetchWithMetadata('/health'); await health.response.body?.cancel();
        const unread = await client.fetchStreamWithMetadata('/stream-idle');
        const reading = await client.fetchStreamWithMetadata('/stream-idle');
        const r = reading.response.body.getReader(); await r.read();
        const closing = code(r.read()); await client.close();
        return { cookie: upstreamHeaders.find(([name]: [string, string]) => name === 'set-cookie')?.[1], aborted, setup, idleCode, excess, health: health.response.status, closing: await closing, unread: await code(unread.response.body.getReader().read()) };
      } finally { await client.close(); }
    }, values);
    expect(result).toEqual({ cookie: 'stream=synthetic; Path=/; HttpOnly', aborted: 'cancelled', setup: 'setupTimeout', idleCode: 'idleTimeout', excess: 'resourceLimit', health: 200, closing: 'closed', unread: 'closed' });
  });
});
