import { describe, expect, it } from "vitest";
import {
  ALL_HARNESSES,
  SUPPORTS_EFFORT_SELECTION,
  SUPPORTS_MODEL_SELECTION,
} from "./harnessDisplay";
import { DEFAULT_EFFORT, DEFAULT_MODEL, EFFORT_OPTIONS, MODEL_OPTIONS } from "./agentSelection";

/// The capability fact ("does harness H support axis A") is encoded three times
/// — the capability map, the option list (empty ⇒ unsupported), and the default
/// (undefined ⇒ unsupported) — across two files, intentionally mirroring
/// different sources. Nothing else enforces that the three agree, and the lists
/// are designed to be hand-edited as models ship/sunset. These invariants fail
/// closed on a desync instead of shipping a broken picker: a capability/list
/// mismatch renders a picker with no options, and a default outside its list
/// binds an orphan value the `<select>` never visibly selects.
describe("agentSelection capability tables stay consistent", () => {
  for (const harness of ALL_HARNESSES) {
    it(`${harness}: model capability map, list, and default agree`, () => {
      const supported = SUPPORTS_MODEL_SELECTION[harness];
      expect(MODEL_OPTIONS[harness].length > 0).toBe(supported);
      expect(DEFAULT_MODEL[harness] !== undefined).toBe(supported);
      if (DEFAULT_MODEL[harness] !== undefined) {
        expect(MODEL_OPTIONS[harness].map((option) => option.value)).toContain(
          DEFAULT_MODEL[harness],
        );
      }
    });

    it(`${harness}: effort capability map, list, and default agree`, () => {
      const supported = SUPPORTS_EFFORT_SELECTION[harness];
      expect(EFFORT_OPTIONS[harness].length > 0).toBe(supported);
      expect(DEFAULT_EFFORT[harness] !== undefined).toBe(supported);
      if (DEFAULT_EFFORT[harness] !== undefined) {
        expect(EFFORT_OPTIONS[harness].map((option) => option.value)).toContain(
          DEFAULT_EFFORT[harness],
        );
      }
    });
  }
});
