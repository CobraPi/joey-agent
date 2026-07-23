import { defineConfig } from 'vite';

// Backend base URL for local dev — override with SPECKIT_UI_BACKEND if the
// backend isn't running on the default port (see crates/joey-speckit-ui).
const backend = process.env.SPECKIT_UI_BACKEND ?? 'http://127.0.0.1:4173';

export default defineConfig({
  root: '.',
  server: {
    port: 5173,
    proxy: {
      '/api': {
        target: backend,
        changeOrigin: true,
        ws: true,
      },
    },
  },
  build: {
    outDir: 'dist',
    target: 'es2020',
  },
});
