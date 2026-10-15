import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  build: {
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL('./examples/minimal/index.html', import.meta.url)),
        lifecycle: fileURLToPath(new URL('./examples/minimal/lifecycle.html', import.meta.url)),
      },
    },
  },
});
