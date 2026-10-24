import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: 'tests',
  testIgnore: '**/*.soak.spec.ts',
  timeout: 30_000,
  // Hosted runners share CPU and the live N0 relay across browser projects.
  // Isolate test cases, not concurrency deliberately exercised within a test.
  // The opt-in soak config overrides this with six concurrent workloads.
  workers: process.env.CI ? 1 : undefined,
  globalSetup: './tests/global-setup.mjs',
  use: { baseURL: process.env.LOCHO_BROWSER_URL ?? 'http://127.0.0.1:4173' },
  webServer: {
    command: 'python3 -m http.server 4173 --bind 127.0.0.1',
    cwd: process.env.LOCHO_BROWSER_DIST ?? 'dist/browser',
    url: process.env.LOCHO_BROWSER_URL ?? 'http://127.0.0.1:4173',
    reuseExistingServer: !process.env.LOCHO_BROWSER_DIST && !process.env.CI,
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
    { name: 'firefox', use: { ...devices['Desktop Firefox'] } },
    { name: 'webkit', use: { ...devices['Desktop Safari'] } },
  ],
});
