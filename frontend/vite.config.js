import { defineConfig } from 'vite';

export default defineConfig({
  root: '.',
  build: {
    outDir: 'dist',
    rollupOptions: {
      input: {
        main: 'index.html',
        publish: 'publisher.html',
      },
    },
  },
  server: {
    proxy: {
      '/config': 'http://localhost:8080',
    },
  },
});
