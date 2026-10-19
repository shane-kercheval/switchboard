/**
 * Stand-in Antigravity models for tests that need a kind of model rather than
 * a particular one: two that take low, medium and high, one that takes only
 * medium, and one that takes no effort at all. Google adds and retires
 * Antigravity models without a CLI release, and tests that used real models as
 * examples had to be rewritten each time. Real ids belong only in tests about
 * the built-in defaults.
 *
 * Vitest hoists `vi.mock` above a file's imports, so load this inside the
 * mock's factory rather than importing it at the top:
 *
 *   vi.mock("$lib/antigravityModels.json", async (importOriginal) =>
 *     (await import("<relative path>/tests/fixtures/antigravityModels")).withStandIns(importOriginal),
 *   );
 */
export const STAND_IN_ANTIGRAVITY_MODELS = [
  { value: "three-level-model", label: "Three Level Model", efforts: ["low", "medium", "high"] },
  {
    value: "other-three-level-model",
    label: "Other Three Level Model",
    efforts: ["low", "medium", "high"],
  },
  { value: "medium-only-model", label: "Medium Only Model", efforts: ["medium"] },
  { value: "no-effort-model", label: "No Effort Model", efforts: null },
];

type ModelTableModule = { default: { models: readonly unknown[] } };

/** The real model table with the stand-ins appended after its own models. */
export async function withStandIns(importOriginal: <T>() => Promise<T>): Promise<ModelTableModule> {
  const actual = await importOriginal<ModelTableModule>();
  return {
    ...actual,
    default: {
      ...actual.default,
      models: [...actual.default.models, ...STAND_IN_ANTIGRAVITY_MODELS],
    },
  };
}
