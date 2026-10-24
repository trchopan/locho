import { defineConfig } from '@playwright/test';
import base from './playwright.config';

export default defineConfig(base, {
  testMatch: '**/*.soak.spec.ts',
  testIgnore: [],
  workers: 6,
  fullyParallel: true,
  retries: 0,
  outputDir: 'test-results/soak',
  // No traces/screenshots/videos: these can capture bearer tickets and session metadata.
  use: { trace: 'off', screenshot: 'off', video: 'off' },
  reporter: [['list'], ['json', { outputFile: 'test-results/soak-results.json' }]],
});
