import { defineConfig } from 'vite';
import { svelte, vitePreprocess } from '@sveltejs/vite-plugin-svelte';

// https://vitejs.dev/config/
export default defineConfig({
  base: './',
  plugins: [svelte({ preprocess: vitePreprocess() })],
  // Tauri expects a fixed port in dev mode
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    // Tauri reads the built files from dist/
    outDir: 'dist',
    target: ['es2021', 'chrome100', 'safari13'],
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});
