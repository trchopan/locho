import { test, expect } from '@playwright/test';

const enabled = process.env.LOCHO_N0_INTEGRATION === '1';

test.describe('official N0 relay integration', () => {
  test.skip(!enabled, 'N0 integration is enabled only by the controlled CI fixture');

  test('reaches the real host and returns the health body through a relay', async ({ page }) => {
    await page.goto('/');
    await page.locator('#host').fill(process.env.LOCHO_HOST_ID!);
    await page.locator('#capability').fill(process.env.LOCHO_CAPABILITY!);
    await page.getByRole('button', { name: 'Connect and GET /health' }).click();
    await expect(page.locator('#output')).toContainText(
      `peer=connected/${process.env.LOCHO_EXPECTED_PATH}`,
      { timeout: 30_000 },
    );
    await expect(page.locator('#output')).toContainText('api=200');
    await expect(page.locator('#output')).toContainText(process.env.LOCHO_EXPECTED_BODY!);
  });

  test('rejects a wrong capability', async ({ page }) => {
    await page.goto('/');
    await page.locator('#host').fill(process.env.LOCHO_HOST_ID!);
    await page.locator('#capability').fill(process.env.LOCHO_CAPABILITY!.replace(/[^:]+$/, 'wrong-secret'));
    await page.getByRole('button', { name: 'Connect and GET /health' }).click();
    await expect(page.locator('#output')).toContainText('serviceRejected', { timeout: 30_000 });
  });
});
