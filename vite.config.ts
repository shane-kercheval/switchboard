import { defineConfig, configDefaults } from "vitest/config";
import { playwright } from "@vitest/browser-playwright";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

const host = process.env.TAURI_DEV_HOST;

// `*.browser.test.ts` specs run in real WebKit (Vitest browser mode) for the
// layout-coupled behavior jsdom can't see; everything else stays in the fast
// jsdom project. See docs/implementation_plans/2026-06-08-webkit-component-tests.md.
const BROWSER_SPECS = ["**/*.browser.{test,spec}.ts"];

export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  resolve: {
    alias: {
      $lib: path.resolve(__dirname, "./src/lib"),
    },
  },
  clearScreen: false,
  server: {
    port: Number(process.env.VITE_DEV_PORT) || 1420,
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
    projects: [
      {
        // Fast default suite: logic + non-layout component behavior under jsdom.
        extends: true,
        // jsdom runs under Node, so force Svelte's *client* build via the
        // "browser" export condition. Scoped to this project only — the real
        // browser project resolves browser conditions natively, where injecting
        // it again is redundant and a needless resolution-ambiguity risk.
        resolve: { conditions: ["browser"] },
        test: {
          name: "jsdom",
          globals: true,
          environment: "jsdom",
          setupFiles: ["./tests/setup.ts"],
          include: ["src/**/*.{test,spec}.{ts,svelte}", "tests/**/*.{test,spec}.ts"],
          // Browser specs are partitioned out so no file double-runs.
          exclude: [...configDefaults.exclude, ...BROWSER_SPECS],
          // 15s ceiling (vs. Vitest's 5s default) — CI runners cold-start
          // Svelte component imports unpredictably and the suite saw sporadic
          // timeout failures at the default. Lower this only if a fast,
          // reliable component-mount path lands; flake recurrence is worse
          // than the wider headroom.
          testTimeout: 15000,
        },
      },
      {
        // Real-WebKit suite: layout/scroll/measurement behavior jsdom can't see.
        extends: true,
        test: {
          name: "browser",
          globals: true,
          setupFiles: ["./tests/browser-setup.ts"],
          include: BROWSER_SPECS,
          browser: {
            enabled: true,
            provider: playwright(),
            headless: true,
            instances: [{ browser: "webkit" }],
          },
        },
      },
    ],
  },
});
