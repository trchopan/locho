import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

test.describe('buffered HTTP over N0', () => {
  test.skip(process.env.LOCHO_N0_INTEGRATION !== '1', 'N0 fixture disabled');
  test.beforeEach(async ({ page }) => {
    await page.goto('./lifecycle.html');
    await page.waitForFunction(() => Boolean((window as any).lochoProof));
  });

  test('binary bodies, chunked responses, headers, bodyless and redirects', async ({ page }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const result = await page.evaluate(async (values) => {
      const { LochoClient, LochoTicket } = (window as any).lochoProof;
      const client = await LochoClient.connect({ ticket: await LochoTicket.parse(values.ticket), http: { authority: 'sample-api' } });
      try {
        const echo = await client.fetch('/echo', { method: 'PATCH', body: new Uint8Array([0, 255, 128]), headers: { Authorization: 'Bearer synthetic' } });
        const chunk = await client.fetch('/chunked');
        const empty = await client.fetch('/empty');
        const head = await client.fetch('/echo', { method: 'HEAD' });
        const redirect = await client.fetch('/redirect');
        const missing = await client.fetch('/missing');
        return {
          echo: Array.from(new Uint8Array(await echo.arrayBuffer())), method: echo.headers.get('x-method'), authority: echo.headers.get('x-authority'),
          chunk: Array.from(new Uint8Array(await chunk.arrayBuffer())), repeated: chunk.headers.get('x-test'),
          empty: [empty.status, empty.body === null], head: head.body === null,
          redirect: [redirect.status, redirect.headers.get('location'), redirect.redirected], missing: missing.status,
        };
      } finally { await client.close(); }
    }, values);
    expect(result).toEqual({ echo: [0, 255, 128], method: 'PATCH', authority: 'sample-api', chunk: [0, 255, 1, 128], repeated: 'one, two', empty: [204, true], head: true, redirect: [302, 'https://example.invalid/', false], missing: 404 });
  });

  test('validation, operation cancellation, timeout and concurrency limit', async ({ page }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const result = await page.evaluate(async (values) => {
      const { LochoClient, LochoTicket } = (window as any).lochoProof;
      const client = await LochoClient.connect({ ticket: await LochoTicket.parse(values.ticket) });
      const code = (promise: Promise<any>) => promise.then(() => 'success', (e: any) => e.code);
      try {
        const validation = await Promise.all([
          code(client.fetch('https://example.invalid/')), code(client.fetch('/', { headers: { Host: 'other' } })),
          code(client.fetch('/', { body: 'get-body' })), code(client.fetch('/', { redirect: 'follow' })),
        ]);
        const timeout = await code(client.fetch('/stall', { timeoutMs: 100 }));
        const controller = new AbortController();
        const cancelled = code(client.fetch('/stall', { signal: controller.signal }));
        controller.abort();
        const abortCode = await cancelled;
        const pending = Array.from({ length: 16 }, () => code(client.fetch('/stall', { signal: controller.signal })));
        await Promise.all(pending); // already-aborted requests must not hold permits
        const controllers = Array.from({ length: 16 }, () => new AbortController());
        const busy = controllers.map((c) => code(client.fetch('/stall', { signal: c.signal })));
        await new Promise((resolve) => setTimeout(resolve, 100));
        const excess = await code(client.fetch('/health'));
        controllers.forEach((c) => c.abort());
        await Promise.all(busy);
        const healthy = (await client.fetch('/health')).status;
        return { validation, timeout, abortCode, excess, healthy };
      } finally { await client.close(); }
    }, values);
    expect(result).toEqual({ validation: ['invalidPath', 'invalidHeader', 'invalidBody', 'unsupportedOption'], timeout: 'timeout', abortCode: 'cancelled', excess: 'resourceLimit', healthy: 200 });
  });

  test('reference notes API health login CRUD and logout', async ({ page }) => {
    test.skip(!process.env.LOCHO_REFERENCE_API, 'reference API checkout not configured');
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const result = await page.evaluate(async (values) => {
      const { LochoClient, LochoTicket } = (window as any).lochoProof;
      const client = await LochoClient.connect({ ticket: await LochoTicket.parse(values.notesTicket), http: { authority: 'sample-api' } });
      try {
        const health = await (await client.fetch('/health')).json();
        const login = await (await client.fetch('/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ email: 'browser@example.test', password: 'synthetic-browser-password' }) })).json();
        const headers = { Authorization: `Bearer ${login.token}`, 'Content-Type': 'application/json' };
        const auth = { Authorization: `Bearer ${login.token}` };
        const me = await (await client.fetch('/api/me', { headers })).json();
        const created = await client.fetch('/api/notes', { method: 'POST', headers, body: JSON.stringify({ title: 'Browser HTTP', body: 'UTF-8 🌍' }) });
        const { note } = await created.json();
        const update = await (await client.fetch(`/api/notes/${note.id}`, { method: 'PATCH', headers, body: JSON.stringify({ body: 'edited' }) })).json();
        const list = await (await client.fetch('/api/notes', { headers })).json();
        const deleted = await client.fetch(`/api/notes/${note.id}`, { method: 'DELETE', headers: auth });
        const logout = await client.fetch('/auth/logout', { method: 'POST', headers: auth });
        const expired = await client.fetch('/api/me', { headers });
        return { health, email: me.user.email, created: created.status, body: update.note.body, found: list.notes.some((n: any) => n.id === note.id), deleted: deleted.status, logout: logout.status, expired: expired.status };
      } finally { await client.close(); }
    }, values);
    expect(result).toEqual({ health: { service: 'locho-notes', apiVersion: 1 }, email: 'browser@example.test', created: 201, body: 'edited', found: true, deleted: 204, logout: 204, expired: 401 });
  });
});
