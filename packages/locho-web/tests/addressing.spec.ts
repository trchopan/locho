import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

test.describe('bounded relay-hint refresh', () => {
  test.skip(process.env.LOCHO_N0_INTEGRATION !== '1', 'N0 fixture disabled');

  test('explicit relay remains usable when discovery is unavailable', async ({ page }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    let lookups = 0;
    await page.route(values.discoveryUrl, async (route) => {
      lookups++;
      await route.fulfill({ status: 503, body: 'temporary discovery failure' });
    });
    await page.goto('./lifecycle.html');
    await page.waitForFunction(() => Boolean((window as any).lochoProof));
    const result = await page.evaluate(async (values) => {
      const { LochoClient } = (window as any).lochoProof;
      const client = await LochoClient.connect({ hostId: values.hostId, peerRelayUrl: values.peerRelayUrl });
      try {
        const response = await client.health(values.capability);
        return { status: response.status, body: (await response.text()).trim() };
      } finally { await client.close(); }
    }, values);
    expect(lookups).toBeGreaterThan(0);
    expect(result).toEqual({ status: 200, body: values.expectedBody });
  });

  test('abort during discovery cancels startup and leaves later clients usable', async ({ page }) => {
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    let observed!: () => void;
    const lookupStarted = new Promise<void>((resolve) => { observed = resolve; });
    await page.route(values.discoveryUrl, async (route) => {
      observed();
      await route.fulfill({ status: 503, body: 'temporary discovery failure' });
    });
    await page.goto('./lifecycle.html');
    await page.waitForFunction(() => Boolean((window as any).lochoProof));
    await page.evaluate((values) => {
      const { LochoClient } = (window as any).lochoProof;
      const abort = new AbortController();
      (window as any).lookupAbort = abort;
      (window as any).lookupPending = LochoClient.connect({ hostId: values.hostId, peerRelayUrl: values.peerRelayUrl, signal: abort.signal })
        .then(async (client: any) => { await client.close(); return 'success'; }, (error: any) => error.code);
    }, values);
    await lookupStarted;
    expect(await page.evaluate(async () => {
      (window as any).lookupAbort.abort();
      return await (window as any).lookupPending;
    })).toBe('cancelled');
    await page.unroute(values.discoveryUrl);
    const healthy = await page.evaluate(async (values) => {
      const { LochoClient } = (window as any).lochoProof;
      const client = await LochoClient.connect({ hostId: values.hostId, peerRelayUrl: values.peerRelayUrl });
      try { return (await client.health(values.capability)).status; }
      finally { await client.close(); }
    }, values);
    expect(healthy).toBe(200);
  });
});
