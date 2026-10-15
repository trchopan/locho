import { test, expect } from '@playwright/test';

test('production proof page loads', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveTitle('Locho browser proof');
  await expect(page.getByRole('heading', { name: 'Locho browser proof' })).toBeVisible();
});

test('real host health proof when credentials are supplied', async ({ page }) => {
  test.skip(!process.env.LOCHO_HOST_ID || !process.env.LOCHO_CAPABILITY, 'real relay fixture not configured');
  await page.goto('/');
  await page.locator('#host').fill(process.env.LOCHO_HOST_ID!);
  await page.locator('#capability').fill(process.env.LOCHO_CAPABILITY!);
  await page.getByRole('button', { name: 'Connect and GET /health' }).click();
  await expect(page.locator('#output')).toContainText('api=200', { timeout: 20_000 });
});
