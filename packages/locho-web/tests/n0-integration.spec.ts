import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

const enabled = process.env.LOCHO_N0_INTEGRATION === '1';
const fixtureFile = process.env.LOCHO_N0_FIXTURE_FILE;

function fixture() {
  if (!fixtureFile) throw new Error('LOCHO_N0_FIXTURE_FILE is required for N0 integration');
  return JSON.parse(readFileSync(fixtureFile, 'utf8'));
}

test.describe('official N0 relay integration', () => {
  test.skip(!enabled, 'N0 integration is enabled only by the controlled CI fixture');

  test('reaches the real host and returns the health body through a relay', async ({ page }) => {
    const values = fixture();
    await page.goto('/');
    await page.locator('#host').fill(values.hostId);
    await page.locator('#capability').fill(values.capability);
    await page.getByRole('button', { name: 'Connect and GET /health' }).click();
    await expect(page.locator('#output')).toContainText(
      `peer=connected/${values.expectedPath}`,
      { timeout: 30_000 },
    );
    await expect(page.locator('#output')).toContainText('api=200');
    await expect(page.locator('#output')).toContainText(values.expectedBody);
  });

  test('rejects a wrong capability', async ({ page }) => {
    const values = fixture();
    await page.goto('/');
    await page.locator('#host').fill(values.hostId);
    await page.locator('#capability').fill(values.capability.replace(/[^:]+$/, 'wrong-secret'));
    await page.getByRole('button', { name: 'Connect and GET /health' }).click();
    await expect(page.locator('#output')).toContainText('serviceRejected', { timeout: 30_000 });
  });
});
