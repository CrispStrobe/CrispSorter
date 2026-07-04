import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import { readFileSync } from "node:fs";

const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf-8"));

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [sveltekit()],

  define: {
    'process.env': {},
    'process.version': '"v16.0.0"',
    __APP_VERSION__: JSON.stringify(pkg.version),
  },

  // @huggingface/transformers uses WASM + Web Workers — must not be pre-bundled by Vite
  optimizeDeps: {
    exclude: ['@huggingface/transformers'],
  },

  build: {
    rollupOptions: {
      output: {
        /** @param {string} id */
        manualChunks(id) {
          // Split heavy vendor deps into separate chunks so the main
          // bundle stays lean and these only load when needed.
          if (id.includes('node_modules/pdfjs-dist')) return 'vendor-pdfjs';
          if (id.includes('node_modules/mammoth')) return 'vendor-mammoth';
          if (id.includes('node_modules/tesseract.js')) return 'vendor-tesseract';
          if (id.includes('node_modules/katex')) return 'vendor-katex';
          if (id.includes('node_modules/deep-chat')) return 'vendor-deep-chat';
          if (id.includes('node_modules/@mlc-ai/web-llm')) return 'vendor-webllm';
          if (id.includes('node_modules/@huggingface/transformers')) return 'vendor-hf-transformers';
        },
      },
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: false,
    hmr: undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
