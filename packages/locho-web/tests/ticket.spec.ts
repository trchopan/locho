import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

const vectors = JSON.parse(readFileSync(new URL('../../../crates/locho-core/tests/vectors/tickets-v1.json', import.meta.url), 'utf8'));

test('shared ticket vectors through production WASM', async ({ page }) => {
  await page.goto('/lifecycle.html');
  await page.waitForFunction(() => Boolean((window as any).lochoProof));
  const results = await page.evaluate(async (vectors) => {
    const { LochoTicket, LochoClient } = (window as any).lochoProof;
    for (const vector of vectors.valid) {
      const ticket = await LochoTicket.parse(vector.ticket);
      if (ticket.export() !== vector.canonical || ticket.metadata.hostId !== vector.host_id) return false;
      if (String(ticket) !== 'LochoTicket([REDACTED])' || JSON.stringify(ticket) !== '"LochoTicket([REDACTED])"') return false;
      const mixed = await LochoClient.connect({ ticket, hostId: vector.host_id }).catch((e: any) => e.code);
      if (mixed !== 'conflictingOptions') return false;
      if (ticket.metadata.serviceType === 'http') {
        const code = await ticket.connect().catch((e: any) => e.code);
        if (code !== 'unsupportedService') return false;
      }
    }
    for (const value of vectors.invalid) {
      const code = await LochoTicket.parse(value).then(() => 'accepted', (e: any) => e.code);
      if (code !== 'invalidTicket') return false;
    }
    return true;
  }, vectors);
  expect(results).toBe(true);
});

test('CLI ticket connects to N0 host and owns its capability', async ({ page }) => {
  test.skip(process.env.LOCHO_N0_INTEGRATION !== '1', 'N0 fixture disabled');
  const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
  await page.goto('/lifecycle.html');
  await page.waitForFunction(() => Boolean((window as any).lochoProof));
  const result = await page.evaluate(async (values) => {
    const { LochoTicket } = (window as any).lochoProof;
    const ticket = await LochoTicket.parse(values.ticket);
    const client = await ticket.connect();
    try {
      const response = await client.health();
      const conflict = await client.health(values.capability).catch((e: any) => e.code);
      return { status: response.status, body: (await response.text()).trim(), path: client.status.path, conflict };
    } finally { await client.close(); }
  }, values);
  expect(result).toEqual({ status: 200, body: values.expectedBody, path: 'relay', conflict: 'conflictingOptions' });
});
