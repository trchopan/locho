import { test as base, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';

const enabled = process.env.LOCHO_N0_INTEGRATION === '1';
const fixtureFile = process.env.LOCHO_N0_FIXTURE_FILE;

const test = base.extend<{ transportDiagnostics: void }>({
  transportDiagnostics: [async ({ page }, use, testInfo) => {
    // Counts only: never retain URLs, headers, frames, or capabilities in diagnostics.
    const counts = { websockets: 0, socketErrors: 0, closedSockets: 0, pageErrors: 0 };
    page.on('websocket', (socket) => {
      counts.websockets += 1;
      socket.on('socketerror', () => { counts.socketErrors += 1; });
      socket.on('close', () => { counts.closedSockets += 1; });
    });
    page.on('pageerror', () => { counts.pageErrors += 1; });
    await use();
    await testInfo.attach('browser-transport-counts', {
      body: JSON.stringify(counts), contentType: 'application/json',
    });
  }, { auto: true }],
});

function fixture() {
  if (!fixtureFile) throw new Error('LOCHO_N0_FIXTURE_FILE is required for N0 integration');
  return JSON.parse(readFileSync(fixtureFile, 'utf8'));
}

async function expectBrowserDiscovery(page: Page, url: string) {
  await expect.poll(
    async () => page.evaluate(async (discoveryUrl) => {
      try {
        const response = await fetch(discoveryUrl, { cache: 'no-store' });
        return response.status;
      } catch {
        return 0;
      }
    }, url),
    { timeout: 15_000, intervals: [500, 1_000, 2_000] },
  ).toBe(200);
}

test.describe('official N0 relay integration', () => {
  test.skip(!enabled, 'N0 integration is enabled only by the controlled CI fixture');

  for (const addressing of ['explicit', 'discovery', 'dotted', 'stale'] as const) {
    test(`reaches the real host through a relay with ${addressing} addressing`, async ({ page }) => {
      const values = fixture();
      const relay = new URL(values.peerRelayUrl);
      if (addressing === 'dotted') relay.hostname += '.';
      const hint = addressing === 'discovery' ? ''
        : addressing === 'stale' ? 'https://127.0.0.1:1/' : relay.toString();
      await page.goto('./');
      if (addressing === 'discovery' || addressing === 'stale') {
        await expectBrowserDiscovery(page, values.discoveryUrl);
      }
      await page.locator('#host').fill(values.hostId);
      await page.locator('#peerRelay').fill(hint);
      await page.locator('#capability').fill(values.capability);
      await page.getByRole('button', { name: 'Connect and GET /health' }).click();
      // Fail immediately on a terminal adapter error; never retry an HTTP request.
      await expect(page.locator('#output')).not.toHaveText('connecting', { timeout: 27_000 });
      await expect(page.locator('#output')).toHaveText(
        `peer=connected/${values.expectedPath}\napi=200\n${values.expectedBody}\n`,
      );
    });
  }

  test('stale relay hints recover from a transient discovery failure without replaying HTTP', async ({ page, request }, testInfo) => {
    const values = fixture();
    const authority = `discovery-retry-${testInfo.project.name}`;
    let lookups = 0;
    await page.route(values.discoveryUrl, async (route) => {
      lookups += 1;
      if (lookups <= 2) {
        await route.fulfill({ status: 503, body: 'temporary discovery failure' });
      } else {
        await route.continue();
      }
    });
    await page.goto('./lifecycle.html');
    await page.waitForFunction(() => Boolean((window as any).lochoProof));
    const result = await page.evaluate(async (values) => {
      const { LochoClient } = (window as any).lochoProof;
      const client = await LochoClient.connect({
        hostId: values.hostId, peerRelayUrl: 'https://127.0.0.1:1/',
      });
      try {
        const response = await client.health(values.capability, values.authority);
        return { status: response.status, body: (await response.text()).trim() };
      } finally {
        await client.close();
      }
    }, { ...values, authority });
    expect(lookups).toBeGreaterThanOrEqual(3);
    expect(result).toEqual({ status: 200, body: values.expectedBody });
    const counts = await (await request.get(`${values.controlUrl}/?authority=${authority}`)).json();
    expect(counts.requests).toBe(1);
  });

  test('rejects a wrong capability', async ({ page }) => {
    const values = fixture();
    await page.goto('./');
    await page.locator('#host').fill(values.hostId);
    await page.locator('#peerRelay').fill(values.peerRelayUrl);
    await page.locator('#capability').fill(values.capability.replace(/[^:]+$/, 'wrong-secret'));
    await page.getByRole('button', { name: 'Connect and GET /health' }).click();
    await expect(page.locator('#output')).toContainText('serviceRejected', { timeout: 30_000 });
  });
});
