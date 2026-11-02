import { defineConfig } from "vite";
export default defineConfig({
  server: { port: 1534, strictPort: true },
  optimizeDeps: { entries: ["index.html"] },
  clearScreen: false,
});
