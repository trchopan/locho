import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: 'tests',
  timeout: 30_000,
  use: { baseURL: process.env.LOCHO_BROWSER_URL ?? 'http://127.0.0.1:4173' },
  webServer: {
    command: 'npm run preview -- --host 127.0.0.1',
    url: 'http://127.0.0.1:4173',
    reuseExistingServer: true,
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
    { name: 'firefox', use: { ...devices['Desktop Firefox'] } },
    { name: 'webkit', use: { ...devices['Desktop Safari'] } },
  ],
});
