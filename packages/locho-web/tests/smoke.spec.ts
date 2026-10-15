import { test, expect } from '@playwright/test';

test('production proof page loads', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveTitle('Locho browser proof');
  await expect(page.getByRole('heading', { name: 'Locho browser proof' })).toBeVisible();
});
