import { getCurrentWindow } from "@tauri-apps/api/window";

export function startWindowDrag(event: MouseEvent): void {
  if (event.button !== 0) return;
  if (event.target instanceof Element) {
    const interactive = event.target.closest("a,button,input,select,textarea,[data-tauri-no-drag]");
    if (interactive !== null) return;
  }
  event.preventDefault();
  void getCurrentWindow().startDragging();
}

export function windowDragRegion(node: HTMLElement): { destroy: () => void } {
  node.addEventListener("mousedown", startWindowDrag);
  return {
    destroy: () => node.removeEventListener("mousedown", startWindowDrag),
  };
}
