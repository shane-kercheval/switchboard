import { beforeEach, describe, expect, it, vi } from "vitest";

const startDragging = vi.fn();
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ startDragging }),
}));

// Imported after the mock is registered.
import { startWindowDrag } from "./windowDrag";

function dragEvent(button: number, target: Element): MouseEvent {
  const event = new MouseEvent("mousedown", { button, bubbles: true, cancelable: true });
  Object.defineProperty(event, "target", { value: target, configurable: true });
  return event;
}

beforeEach(() => startDragging.mockClear());

describe("startWindowDrag", () => {
  it("starts dragging on a left-click over a non-interactive region", () => {
    startWindowDrag(dragEvent(0, document.createElement("div")));
    expect(startDragging).toHaveBeenCalledTimes(1);
  });

  it("ignores non-left mouse buttons", () => {
    startWindowDrag(dragEvent(2, document.createElement("div")));
    expect(startDragging).not.toHaveBeenCalled();
  });

  it("does not drag when the target is inside an interactive element", () => {
    const button = document.createElement("button");
    const child = document.createElement("span");
    button.appendChild(child);
    startWindowDrag(dragEvent(0, child));
    expect(startDragging).not.toHaveBeenCalled();
  });

  it("does not drag from a [data-tauri-no-drag] element", () => {
    const el = document.createElement("div");
    el.setAttribute("data-tauri-no-drag", "");
    startWindowDrag(dragEvent(0, el));
    expect(startDragging).not.toHaveBeenCalled();
  });
});
