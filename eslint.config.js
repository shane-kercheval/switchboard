import js from "@eslint/js";
import ts from "typescript-eslint";
import svelte from "eslint-plugin-svelte";
import globals from "globals";
import svelteConfig from "./svelte.config.js";

export default ts.config(
  {
    ignores: ["dist/", "target/", "node_modules/", "crates/", ".github/", "docs/", ".claude/"],
  },
  js.configs.recommended,
  ...ts.configs.recommended,
  ...svelte.configs["flat/recommended"],
  {
    // Allow `_`-prefixed names to mark intentionally-unused params (mock
    // function signatures, callback shapes we must conform to without
    // referencing every argument).
    rules: {
      "no-unused-vars": "off",
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
    },
  },
  {
    // Browser-only: src/ ships to the WebView. Node globals (process, Buffer, etc.)
    // do not exist there — leaking them past lint would mask real bugs since
    // OS-level access belongs on the Rust side via Tauri commands.
    files: ["src/**/*.{ts,svelte,svelte.ts}"],
    languageOptions: {
      globals: { ...globals.browser },
    },
  },
  {
    // Node-only: config files + test runner setup execute under Node.
    files: [
      "*.config.{ts,js,mjs,cjs}",
      "tests/**/*.{ts,js}",
      "vite.config.ts",
      "svelte.config.js",
      "eslint.config.js",
    ],
    languageOptions: {
      globals: { ...globals.node },
    },
  },
  {
    files: ["**/*.svelte", "**/*.svelte.ts"],
    languageOptions: {
      parserOptions: {
        parser: ts.parser,
        svelteConfig,
      },
    },
  },
);
