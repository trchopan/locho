import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';

const durationSeconds = Number(process.env.LOCHO_SOAK_SECONDS ?? 900);
if (!Number.isInteger(durationSeconds) || durationSeconds < 1 || durationSeconds > 3600) {
  throw new Error('LOCHO_SOAK_SECONDS must be an integer in 1..3600');
}
if (process.env.LOCHO_N0_INTEGRATION !== '1') throw new Error('Soak requires LOCHO_N0_INTEGRATION=1');

for (const context of ['page', 'service-worker'] as const) {
  test(`metadata soak in ${context}`, async ({ page, request, browser }, testInfo) => {
    test.setTimeout(durationSeconds * 1000 + 180_000);
    const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
    const authority = `soak-${randomUUID()}`;
    const samples: unknown[] = [];
    let cycles = 0;
    let completed = false;
    let cleanupSucceeded = false;
    const started = Date.now();
    const control = `${values.controlUrl}/?authority=${authority}`;
    await page.goto('./lifecycle.html');
    await page.waitForFunction(() => Boolean((window as any).lochoProof));
    try {
      await page.evaluate(async ({ context, ticket, authority }) => {
        const proof = (window as any).lochoProof;
        if (context === 'page') {
          (window as any).soakOwner = await proof.MetadataSoak.open(ticket, authority);
          return;
        }
        const registration = await navigator.serviceWorker.register(proof.metadataSoakWorkerUrl, { type: 'module' });
        (window as any).soakRegistration = registration;
        const worker = registration.installing ?? registration.waiting ?? registration.active!;
        if (worker.state !== 'activated') await new Promise<void>((resolve, reject) => {
          worker.addEventListener('statechange', () => {
            if (worker.state === 'activated') resolve();
            if (worker.state === 'redundant') reject(new Error('workerInstallationFailed'));
          });
        });
        (window as any).soakSend = (data: unknown) => new Promise((resolve, reject) => {
          const channel = new MessageChannel();
          const timer = setTimeout(() => { channel.port1.close(); reject(new Error('workerBatchTimeout')); }, 120_000);
          channel.port1.onmessage = ({ data }) => {
            clearTimeout(timer); channel.port1.close();
            if (data.failed) reject(new Error('workerWorkloadFailed'));
            else resolve(data.result);
          };
          worker.postMessage(data, [channel.port2]);
        });
        await (window as any).soakSend({ action: 'open', ticket, authority });
      }, { context, ticket: values.ticket, authority });

      const deadline = Date.now() + durationSeconds * 1000;
      do {
        // All browser-side failures use fixed codes, never response metadata.
        const stats = await page.evaluate(async (context) => context === 'page'
          ? (window as any).soakOwner.batch() : (window as any).soakSend({ action: 'batch' }), context);
        cycles = stats.cycles;
        await expect.poll(async () => (await (await request.get(control)).json()).sockets, { timeout: 10_000 }).toBe(0);
        const fixture = await (await request.get(control)).json();
        const jsHeapBytes = context === 'page' ? await page.evaluate(() =>
          (performance as Performance & { memory?: { usedJSHeapSize: number } }).memory?.usedJSHeapSize ?? null) : null;
        samples.push({ elapsedMs: Date.now() - started, cycles: stats.cycles,
          reconnects: stats.reconnects, capacityChecks: stats.capacityChecks, cycleLatencyMs: stats.cycleLatencyMs, upstreamSockets: fixture.sockets,
          fixtureRssBytes: fixture.fixtureRssBytes, jsHeapBytes });
        if (samples.length > 360) samples.splice(1, 1); // Bounded diagnostics; retain baseline and recent trends.
        expect(stats.capacityChecks).toBeGreaterThan(0);
        expect(stats.reconnects).toBeGreaterThan(0);
      } while (Date.now() < deadline);
      expect(cycles).toBeGreaterThanOrEqual(5);
      completed = true;
    } finally {
      try {
        await page.evaluate(async (context) => {
          if (context === 'page') {
            await (window as any).soakOwner?.close(); delete (window as any).soakOwner;
          } else {
            try { await (window as any).soakSend?.({ action: 'close' }); }
            finally { await (window as any).soakRegistration?.unregister(); delete (window as any).soakSend; }
          }
        }, context);
        await expect.poll(async () => (await (await request.get(control)).json()).sockets, { timeout: 10_000 }).toBe(0);
        cleanupSucceeded = true;
      } finally {
        // Construct diagnostics from an allowlist. Never attach fixture values, cookies or arbitrary errors.
        await testInfo.attach('metadata-soak.json', { contentType: 'application/json', body: JSON.stringify({
          browser: testInfo.project.name, browserVersion: browser.version(), platform: process.platform,
          context, requestedDurationSeconds: durationSeconds,
          elapsedMs: Date.now() - started, cycles, completed, cleanupSucceeded, samples,
          memoryCoverage: context === 'page' ? 'JS heap where supported; fixture RSS; not a leak assertion' : 'fixture RSS only; worker heap unavailable',
        }, null, 2) });
      }
    }
  });
}
