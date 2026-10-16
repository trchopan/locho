import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

test.describe('streaming HTTP over N0', () => {
  test.skip(process.env.LOCHO_N0_INTEGRATION !== '1', 'N0 fixture disabled');
  test.beforeEach(async ({ page }) => {
    await page.goto('./lifecycle.html');
    await page.waitForFunction(() => Boolean((window as any).lochoProof));
  });

  test('authenticated reference SSE delivers committed changes beyond setup deadline', async ({ page }) => {
    test.skip(!process.env.LOCHO_REFERENCE_API, 'reference API checkout not configured');
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const result = await page.evaluate(async (values) => {
      const { LochoClient, LochoTicket } = (window as any).lochoProof;
      const client = await LochoClient.connect({ ticket: await LochoTicket.parse(values.notesTicket) });
      try {
        const denied = await client.fetchStream('/api/events');
        const deniedStatus = denied.status; await denied.body.cancel();
        const login = await (await client.fetch('/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ email: 'browser@example.test', password: 'synthetic-browser-password' }) })).json();
        const headers = { Authorization: `Bearer ${login.token}`, 'Content-Type': 'application/json' };
        const response = await client.fetchStream('/api/events', { headers, setupTimeoutMs: 3000 });
        const reader = response.body.getReader();
        let buffered = '';
        async function frame() {
          while (!buffered.includes('\n\n')) {
            const { done, value } = await reader.read();
            if (done) throw new Error('Early stream end');
            buffered += new TextDecoder().decode(value);
          }
          const index = buffered.indexOf('\n\n');
          const out = buffered.slice(0, index); buffered = buffered.slice(index + 2); return out;
        }
        const connected = await frame();
        await new Promise((r) => setTimeout(r, 3100));
        const created = await (await client.fetch('/api/notes', { method: 'POST', headers, body: JSON.stringify({ title: 'Streaming', body: 'private body' }) })).json();
        // Other concurrent matrix tests use this user too; wait for our own note.
        const expected = `event: note.changed\ndata: ${JSON.stringify({ operation: 'created', noteId: created.note.id })}`;
        let event;
        do { event = await frame(); } while (event !== expected);
        const matches = event === expected;
        await reader.cancel();
        await client.fetch(`/api/notes/${created.note.id}`, { method: 'DELETE', headers });
        await client.fetch('/auth/logout', { method: 'POST', headers });
        return { deniedStatus, connected, matches };
      } finally { await client.close(); }
    }, values);
    expect(result).toEqual({ deniedStatus: 401, connected: ': connected', matches: true });
  });

  test('incremental framing, ordinary HTTP statuses and cancellation preserve other requests', async ({ page }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const result = await page.evaluate(async (values) => {
      const { LochoClient, LochoTicket } = (window as any).lochoProof;
      const client = await LochoClient.connect({ ticket: await LochoTicket.parse(values.ticket) });
      try {
        const response = await client.fetchStream('/stream');
        const reader = response.body.getReader();
        const first = new TextDecoder().decode((await reader.read()).value);
        await reader.cancel();
        const chunked = Array.from(new Uint8Array(await (await client.fetchStream('/chunked')).arrayBuffer()));
        const echo = Array.from(new Uint8Array(await (await client.fetchStream('/echo', { method: 'POST', body: new Uint8Array([0, 255]) })).arrayBuffer()));
        const empty = await client.fetchStream('/empty');
        const head = await client.fetchStream('/echo', { method: 'HEAD' });
        const redirect = await client.fetchStream('/redirect'); await redirect.body.cancel();
        const missing = await client.fetchStream('/missing'); await missing.body.cancel();
        return { first, chunked, echo, empty: empty.body === null, head: head.body === null, redirect: redirect.status, missing: missing.status, health: (await client.fetch('/health')).status };
      } finally { await client.close(); }
    }, values);
    expect(result).toEqual({ first: ': connected\n\n', chunked: [0, 255, 1, 128], echo: [0, 255], empty: true, head: true, redirect: 302, missing: 404, health: 200 });
  });

  test('setup/idle deadlines, paused readers, abort after headers and pending-read cancellation', async ({ page }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const result = await page.evaluate(async (values) => {
      const { LochoClient, LochoTicket } = (window as any).lochoProof;
      const client = await LochoClient.connect({ ticket: await LochoTicket.parse(values.ticket) });
      const code = (p: Promise<any>) => p.then(() => 'success', (e) => e.code);
      try {
        const preAbort = new AbortController(); preAbort.abort();
        const validation = await Promise.all([
          code(client.fetchStream('/stream', { signal: preAbort.signal })),
          code(client.fetchStream('/stream', { timeoutMs: 100 })),
          code(client.fetchStream('/stream', { idleTimeoutMs: 0 })),
          code(client.fetchStream('//other')),
        ]);
        const setup = await code(client.fetchStream('/stall', { setupTimeoutMs: 100 }));
        const idle = await client.fetchStream('/stream-idle', { idleTimeoutMs: 100 });
        const reader = idle.body.getReader(); await reader.read();
        await new Promise((r) => setTimeout(r, 250));
        // No active read during the pause; the next read gets its own idle budget.
        const start = performance.now(); const idleCode = await code(reader.read());
        const waited = performance.now() - start >= 70;
        const abort = new AbortController();
        const response = await client.fetchStream('/stream-idle', { signal: abort.signal });
        const a = response.body.getReader(); await a.read();
        const pending = code(a.read()); abort.abort(); const aborted = await pending;
        const cancelResponse = await client.fetchStream('/stream-idle');
        const c = cancelResponse.body.getReader(); await c.read();
        const pendingCancel = c.read(); await c.cancel(); await pendingCancel;
        const truncated = await client.fetchStream('/truncated');
        const t = truncated.body.getReader();
        const truncatedCode = await code((async () => { while (!(await t.read()).done) {} })());
        return { validation, setup, idleCode, waited, aborted, truncatedCode, health: (await client.fetch('/health')).status };
      } finally { await client.close(); }
    }, values);
    expect(result).toEqual({ validation: ['cancelled', 'unsupportedOption', 'invalidTimeout', 'invalidPath'], setup: 'setupTimeout', idleCode: 'idleTimeout', waited: true, aborted: 'cancelled', truncatedCode: 'truncatedResponse', health: 200 });
  });

  test('open bodies retain permits; cancellation and close reclaim even unread bodies', async ({ page }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const result = await page.evaluate(async (values) => {
      const { LochoClient, LochoTicket } = (window as any).lochoProof;
      const client = await LochoClient.connect({ ticket: await LochoTicket.parse(values.ticket) });
      const code = (p: Promise<any>) => p.then(() => 'success', (e) => e.code);
      try {
        const bodies = await Promise.all(Array.from({ length: 16 }, () => client.fetchStream('/stream-idle')));
        const excess = await code(client.fetch('/health'));
        await Promise.all(bodies.map((r) => r.body.cancel()));
        const health = (await client.fetch('/health')).status;
        const unread = await client.fetchStream('/stream-idle');
        const reading = await client.fetchStream('/stream-idle');
        const reader = reading.body.getReader(); await reader.read();
        const pending = code(reader.read());
        await client.close();
        return { excess, health, pending: await pending, unread: await code(unread.body.getReader().read()) };
      } finally { await client.close(); }
    }, values);
    expect(result).toEqual({ excess: 'resourceLimit', health: 200, pending: 'closed', unread: 'closed' });
  });

  test('paused heartbeat stream survives idle budget and cancel releases upstream socket', async ({ page, request }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const authority = `stream-${test.info().project.name}`;
    await page.evaluate(async ({ values, authority }) => {
      const { LochoClient, LochoTicket } = (window as any).lochoProof;
      const client = await LochoClient.connect({ ticket: await LochoTicket.parse(values.ticket), http: { authority } });
      (window as any).streamOwner = client;
      const response = await client.fetchStream('/stream', { idleTimeoutMs: 500 });
      (window as any).streamReader = response.body.getReader();
      await (window as any).streamReader.read();
    }, { values, authority });
    try {
      await expect.poll(async () => (await (await request.get(`${values.controlUrl}/?authority=${authority}`)).json()).active).toBe(true);
      await page.waitForTimeout(700);
      expect(await page.evaluate(async () => {
        const { value } = await (window as any).streamReader.read();
        await (window as any).streamReader.cancel();
        return new TextDecoder().decode(value).includes(': heartbeat');
      })).toBe(true);
      await expect.poll(async () => (await (await request.get(`${values.controlUrl}/?authority=${authority}`)).json()).active).toBe(false);
    } finally { await page.evaluate(() => (window as any).streamOwner.close()); }
  });
});
