import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';

test.describe('browser lifecycle over N0', () => {
  test.skip(process.env.LOCHO_N0_INTEGRATION !== '1', 'N0 fixture disabled');

  test.beforeEach(async ({ page }) => {
    await page.goto('/lifecycle.html');
    await page.waitForFunction(() => Boolean((window as any).lochoProof));
  });

  test('pre-aborted startup and active startup preserve cancelled', async ({ page }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const result = await page.evaluate(async (values) => {
      const { LochoClient } = (window as any).lochoProof;
      const code = (promise: Promise<any>) => promise.then(() => 'success', (error) => error.code);
      const pre = new AbortController();
      pre.abort();
      const preCode = await code(LochoClient.connect({ hostId: values.hostId, signal: pre.signal }));
      // Initialize WASM before testing cancellation of Rust startup.
      const warm = await LochoClient.connect({ hostId: values.hostId, peerRelayUrl: values.peerRelayUrl });
      await warm.close();
      const controller = new AbortController();
      const operation = LochoClient.connect({ hostId: '00'.repeat(32), signal: controller.signal });
      setTimeout(() => controller.abort(), 100);
      return { preCode, activeCode: await code(operation) };
    }, values);
    expect(result).toEqual({ preCode: 'cancelled', activeCode: 'cancelled' });
  });

  for (const mode of ['cancel', 'close', 'interrupt'] as const) {
    test(`${mode} releases an active upstream stream`, async ({ page, request }) => {
      const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
      const authority = `stall-${randomUUID()}`;
      const unhandled: string[] = [];
      page.on('pageerror', (error) => unhandled.push(error.name));
      await page.evaluate(async ({ values, authority }) => {
        const { LochoClient } = (window as any).lochoProof;
        const client = await LochoClient.connect({ hostId: values.hostId, peerRelayUrl: values.peerRelayUrl });
        const controller = new AbortController();
        const operation = client.health(values.capability, authority, controller.signal)
          .then(() => 'success', (error: any) => error.code);
        Object.assign(window, { activeProof: { client, controller, operation } });
      }, { values, authority });
      const statusUrl = `${values.controlUrl}/?authority=${authority}`;
      await expect.poll(async () => (await (await request.get(statusUrl)).json()).active).toBe(true);
      if (mode === 'interrupt') await request.post(`${values.controlUrl}/interrupt?authority=${authority}`);
      const result = await page.evaluate(async ({ mode, values }) => {
        const { client, controller, operation } = (window as any).activeProof;
        // Start independent traffic before cancelling the stalled request.
        const independent = mode === 'cancel'
          ? client.health(values.capability).then((r: Response) => r.text()) : undefined;
        if (mode === 'cancel') controller.abort();
        if (mode === 'close') await Promise.all([client.close(), client.close()]);
        const code = await operation;
        let healthy = '';
        if (independent) healthy = await independent;
        else if (mode !== 'close') healthy = await (await client.health(values.capability)).text();
        await client.close();
        const state = client.status.state;
        const afterClose = await client.health(values.capability).then(() => 'success', (error: any) => error.code);
        return { code, healthy, state, afterClose };
      }, { mode, values });
      expect(result.code).toBe(mode === 'cancel' ? 'cancelled' : mode === 'close' ? 'closed' : 'invalidResponse');
      if (mode !== 'close') expect(result.healthy.trim()).toBe(values.expectedBody);
      expect(result.state).toBe('closed');
      expect(result.afterClose).toBe('closed');
      await expect.poll(async () => (await (await request.get(statusUrl)).json()).active).toBe(false);
      expect((await (await request.get(statusUrl)).json()).requests).toBe(1);
      expect(unhandled).toEqual([]);
    });
  }

  test('pre-aborted health, independent traffic and repeated close cycles', async ({ page, request }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const authority = `pre-${randomUUID()}`;
    const result = await page.evaluate(async (values) => {
      const { LochoClient } = (window as any).lochoProof;
      const results = [];
      for (let cycle = 0; cycle < 3; cycle += 1) {
        const client = await LochoClient.connect({ hostId: values.hostId, peerRelayUrl: values.peerRelayUrl });
        const controller = new AbortController();
        controller.abort();
        const [cancelled, healthy, unavailable] = await Promise.all([
          client.health(values.capability, values.authority, controller.signal).then(() => 'success', (e: any) => e.code),
          client.health(values.capability).then((r: Response) => r.text()),
          client.health(values.unavailableCapability).then(() => 'success', (e: any) => e.code),
        ]);
        await Promise.all([client.close(), client.close()]);
        results.push({ cancelled, healthy: healthy.trim(), unavailable, state: client.status.state });
      }
      return results;
    }, { ...values, authority });
    expect(result).toEqual(Array.from({ length: 3 }, () => ({
      cancelled: 'cancelled', healthy: values.expectedBody, unavailable: 'serviceRejected', state: 'closed',
    })));
    expect((await (await request.get(`${values.controlUrl}/?authority=${authority}`)).json()).requests).toBe(0);
  });
});
