import { defineConfig } from "vite";

// Tauri expects a fixed dev port and serves the built files from ../dist.
export default defineConfig({
  root: "ui",
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    target: "es2022",
    rollupOptions: {
      input: {
        // Relative to the project folder, where every npm script runs.
        main: "ui/index.html",
      },
    },
  },
});
