/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  // Vitest runs under Node and needs the "browser" export condition explicitly so
  // Svelte's client-side mount is loaded instead of the server stub. In normal
  // `vite dev` / `vite build`, leave conditions unset so Vite's defaults apply.
  resolve: {
    alias: {
      $lib: path.resolve(__dirname, "./src/lib"),
    },
    ...(process.env.VITEST ? { conditions: ["browser"] } : {}),
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/crates/**", "**/target/**"],
    },
  },
  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: ["./tests/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,svelte}", "tests/**/*.{test,spec}.ts"],
    // 15s ceiling (vs. Vitest's 5s default) — CI runners cold-start
    // Svelte component imports unpredictably and the suite saw sporadic
    // timeout failures at the default. Lower this only if a fast,
    // reliable component-mount path lands; flake recurrence is worse
    // than the wider headroom.
    testTimeout: 15000,
  },
});
