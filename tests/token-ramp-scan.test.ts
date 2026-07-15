import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

// Guards the neutral-ramp invariants from ui-conventions.md, the only mechanical
// backstop that stops the collapsed ramp from re-accreting a fifteenth gray.
// Two rules, both text-scannable across `src/`:
//
//   1. No opacity modifier on a surface token. `bg-panel/35` composes
//      differently over every parent and yields a shade nobody named — exactly
//      the muddiness the ramp collapse removed.
//   2. `border` is a line, never a fill. `bg-border` (in any form) is banned;
//      rows use `bg-hover`, compact controls use `bg-control-hover`, and
//      pressed/track fills use `bg-active`.
//
// The two-nested-treatments rule (a bordered container's child gets a fill or
// nothing, not both) is deliberately NOT scanned — it needs a human eye and
// lives as a review rule in ui-conventions.md.

// Opacity on a surface token, in any Tailwind form: the shorthand `bg-panel/50`
// and the arbitrary `bg-panel/[0.35]` both violate the rule, so we ban *any*
// slash after these four names — nothing legitimate follows `bg-panel/`.
const SURFACE_OPACITY = /\bbg-(?:surface|panel|raised|border)\//;
// `bg-border` as a fill (covers the bare form and the `/n` form). `border-border`
// (an actual line) does not contain `bg-border`, so it is not matched.
const BORDER_FILL = /\bbg-border\b/;

const SRC_DIR = join(import.meta.dirname, "..", "src");

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...sourceFiles(path));
    } else if (entry.name.endsWith(".svelte") || entry.name.endsWith(".ts")) {
      out.push(path);
    }
  }
  return out;
}

type Violation = { file: string; line: number; rule: string; text: string };

function scan(): Violation[] {
  const violations: Violation[] = [];
  for (const file of sourceFiles(SRC_DIR)) {
    const lines = readFileSync(file, "utf8").split("\n");
    lines.forEach((text, i) => {
      const rel = file.slice(SRC_DIR.length - "src".length);
      if (SURFACE_OPACITY.test(text)) {
        violations.push({
          file: rel,
          line: i + 1,
          rule: "surface-token opacity",
          text: text.trim(),
        });
      }
      if (BORDER_FILL.test(text)) {
        violations.push({ file: rel, line: i + 1, rule: "bg-border fill", text: text.trim() });
      }
    });
  }
  return violations;
}

describe("neutral-ramp scan", () => {
  it("no source file uses a banned neutral-ramp pattern", () => {
    const violations = scan();
    const report = violations.map((v) => `  ${v.file}:${v.line} [${v.rule}] ${v.text}`).join("\n");
    expect(violations, `banned neutral-ramp patterns found:\n${report}`).toEqual([]);
  });

  // Proves the guard actually bites — otherwise a broken regex would let the
  // ramp re-accrete while this test stayed green.
  it("catches a deliberately introduced violation", () => {
    expect(SURFACE_OPACITY.test('class="bg-panel/50"')).toBe(true);
    expect(SURFACE_OPACITY.test('class="bg-surface/30"')).toBe(true);
    // Arbitrary-value opacity is the same violation and must not slip past.
    expect(SURFACE_OPACITY.test('class="bg-panel/[0.35]"')).toBe(true);
    expect(SURFACE_OPACITY.test('class="bg-raised/[.4]"')).toBe(true);
    expect(BORDER_FILL.test('class="bg-border"')).toBe(true);
    expect(BORDER_FILL.test('class="hover:bg-border/60"')).toBe(true);
  });

  // The migrated forms must pass, so the guard doesn't over-reach onto the
  // tokens the ramp actually sanctions.
  it("permits the sanctioned ramp tokens", () => {
    for (const ok of [
      "bg-hover",
      "bg-control-hover",
      "bg-active",
      "bg-panel",
      "bg-focus-soft",
      "border-border",
      "hover:bg-hover",
      "hover:bg-control-hover",
    ]) {
      expect(SURFACE_OPACITY.test(`class="${ok}"`)).toBe(false);
      expect(BORDER_FILL.test(`class="${ok}"`)).toBe(false);
    }
  });
});
