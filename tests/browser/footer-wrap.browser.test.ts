import { beforeEach, expect, test, vi } from "vitest";
import { page } from "vitest/browser";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));
vi.mock("$lib/native", () => ({ copyText: vi.fn(async () => undefined) }));

import { mountTranscript } from "./mount";
import { registerAgent, seedTurns, resetState } from "./harness";
import { ALICE, PROJECT_ID, agentTurn, longText, textItem } from "./fixtures";

const LONG_MODEL = "claude-opus-4-8-20260109-experimental-extra-long";

// The meta row pins the expand/collapse toggle LEFT and the model · timestamp ·
// copy cluster RIGHT. In a narrow fan-out column or small window the cluster must
// stay inside the column — its text wraps (model over timestamp) and truncates
// with `…` rather than overflow into the neighbouring column — while the toggle
// and copy button never squish. jsdom can't see this (no layout / no wrapping);
// the original bug was a `whitespace-nowrap` that forced the footer wider than
// its column.

beforeEach(() => {
  resetState();
});

test("a long model in a narrow column wraps instead of overflowing horizontally", async () => {
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    agentTurn({
      id: "agent-1",
      agentId: ALICE.id,
      items: [textItem("done")],
      model: LONG_MODEL,
    }),
  ]);

  // A deliberately narrow mount (like one column of a 3–4 way fan-out).
  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE], width: 240 });

  // The model footer is laid out (hover-revealed = opacity-0, but real geometry).
  await expect.element(page.getByTestId("message-model")).toBeInTheDocument();

  // No horizontal overflow of the transcript: the footer wrapped to fit the
  // narrow container instead of spilling past its right edge.
  await expect
    .poll(() => {
      const c = page.getByTestId("unified-transcript").element() as HTMLElement;
      return c.scrollWidth - c.clientWidth;
    })
    .toBeLessThanOrEqual(1);
});

test("the toggle stays left of the cluster and the model truncates when room runs out", async () => {
  // A clipped (→ toggled) response with a long model: the toggle is pinned LEFT
  // and the model/timestamp/copy cluster is pinned RIGHT. In a narrow column the
  // toggle must not be pushed/squished by the cluster, and the model must truncate
  // with an ellipsis rather than overflow or wrap endlessly.
  await registerAgent(ALICE);
  seedTurns(ALICE.id, [
    // Earlier (non-last-block → clipped → owns a toggle) response, with the model.
    agentTurn({
      id: "agent-early",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:01Z",
      items: [textItem(longText())],
      model: LONG_MODEL,
    }),
    // A later response so the early one is NOT the last block.
    agentTurn({
      id: "agent-late",
      agentId: ALICE.id,
      at: "2026-05-16T00:00:09Z",
      items: [textItem("short latest")],
    }),
  ]);

  mountTranscript({ projectId: PROJECT_ID, agents: [ALICE], width: 300 });

  const toggle = page.getByTestId("turn-preview-toggle");
  await expect.element(toggle).toBeInTheDocument();
  await expect.element(page.getByTestId("message-model")).toBeInTheDocument();

  // The toggle sits entirely to the LEFT of the model cluster (left-pinned layout).
  await expect
    .poll(() => {
      const t = (toggle.element() as HTMLElement).getBoundingClientRect();
      const m = (
        page.getByTestId("message-model").element() as HTMLElement
      ).getBoundingClientRect();
      return t.right <= m.left + 1;
    })
    .toBe(true);

  // The model is ellipsis-truncated (rendered width < its full text width), not
  // overflowing or breaking onto many lines.
  await expect
    .poll(() => {
      const m = page.getByTestId("message-model").element() as HTMLElement;
      return m.scrollWidth - m.clientWidth;
    })
    .toBeGreaterThan(1);

  // Still no horizontal overflow of the column.
  const c = page.getByTestId("unified-transcript").element() as HTMLElement;
  expect(c.scrollWidth - c.clientWidth).toBeLessThanOrEqual(1);
});
