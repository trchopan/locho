import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

test('watch-backed observers cancel independently and retain terminal status', async ({ page }) => {
  test.skip(process.env.LOCHO_N0_INTEGRATION !== '1', 'N0 fixture disabled');
  const values = JSON.parse(readFileSync(process.env.LOCHO_N0_FIXTURE_FILE!, 'utf8'));
  await page.goto('./lifecycle.html');
  await page.waitForFunction(() => Boolean((window as any).lochoProof));
  const result = await page.evaluate(async (values) => {
    const { LochoTicket } = (window as any).lochoProof;
    const client = await (await LochoTicket.parse(values.ticket)).connect();
    const states: any[] = [];
    const independent: any[] = [];
    const off = client.subscribeStatus((s: any) => states.push(s));
    let terminal!: () => void;
    const closed = new Promise<void>((resolve) => { terminal = resolve; });
    const other = client.subscribeStatus((s: any) => { independent.push(s); if (s.state === 'closed') terminal(); });
    await new Promise((resolve) => setTimeout(resolve, 100));
    await off(); const count = states.length;
    const healthy = (await client.fetch('/health')).status;
    const slow = client.subscribeStatus(() => new Promise(() => {}));
    await new Promise((resolve) => setTimeout(resolve, 20));
    await slow();
    let callbackError = '';
    const broken = client.subscribeStatus(() => { throw new Error('listener'); }, { onError: (e: any) => { callbackError = e.code; } });
    await new Promise((resolve) => setTimeout(resolve, 20)); await broken();
    const observers = Array.from({ length: 15 }, () => client.subscribeStatus(() => {}));
    let cap = '';
    try { client.subscribeStatus(() => {}); } catch (e: any) { cap = e.code; }
    await Promise.all(observers.map((off: any) => off()));
    const controller = new AbortController();
    const aborted = client.subscribeStatus(() => {}, { signal: controller.signal });
    controller.abort(); await aborted();
    let slowCalls = 0;
    const closingSlow = client.subscribeStatus(() => {
      slowCalls++;
      if (slowCalls === 1) return new Promise(() => {});
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    await client.close(); await closed; await other();
    await closingSlow();
    const final: any[] = [];
    const late = client.subscribeStatus((s: any) => final.push(s));
    await new Promise((resolve) => setTimeout(resolve, 20)); await late();
    return { healthy, unchanged: states.length === count, first: states[0].state, last: independent.at(-1).state,
      revisions: independent.every((s: any, i: number) => !i || s.revision > independent[i-1].revision),
      fields: 'connectionErrorCode' in states[0] && 'errorCode' in states[0], cap, callbackError, late: final.map((s) => s.state), slowTerminal: slowCalls >= 2 };
  }, values);
  expect(result).toEqual({ healthy: 200, unchanged: true, first: 'connected', last: 'closed', revisions: true,
    fields: true, cap: 'resourceLimit', callbackError: 'internal', late: ['closed'], slowTerminal: true });
});
