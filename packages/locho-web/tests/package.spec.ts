import { test, expect } from '@playwright/test';

test('external package supports explicit WASM asset initialization', async ({ page }) => {
  test.skip(!process.env.LOCHO_BROWSER_DIST, 'external consumer only');
  const wasmResponses: Array<{ status: number; mime: string }> = [];
  page.on('response', (response) => {
    if (new URL(response.url()).pathname.endsWith('.wasm')) {
      wasmResponses.push({ status: response.status(), mime: response.headers()['content-type'] ?? '' });
    }
  });
  await page.goto('./initialize.html');
  await expect(page.locator('#result')).toHaveText('initialized');
  expect(wasmResponses).toEqual([{ status: 200, mime: 'application/wasm' }]);
});
