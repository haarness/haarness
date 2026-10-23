export type OrderPoint = { x: number; y: number };

/** Internal row sorting stays in the renderer. Native HTML dragging hands
 * mouse input to an OS drag loop, whose drop delivery differs from automated
 * Chromium drag interception. File attachments still use native file drops. */
export function createPointerOrdering(callbacks: {
  hover: (point: OrderPoint) => void;
  drop: (point: OrderPoint) => (() => void) | undefined;
  clear: () => void;
}) {
  let cancel: (() => void) | undefined;

  function begin(event: PointerEvent, element: HTMLElement, handle: HTMLElement, start: () => void) {
    if (event.button !== 0 || !event.isPrimary) return;
    cancel?.();
    const origin = { x: event.clientX, y: event.clientY };
    let point = origin;
    let active = false;
    let frame = 0;
    let preview: HTMLDivElement | undefined;
    let previousTime = 0;
    const position = () => {
      if (preview) preview.style.transform = `translate(${point.x + 12}px, ${point.y + 12}px)`;
      callbacks.hover(point);
    };
    const scroll = (now: number) => {
      if (!element.isConnected) { finish(); return; }
      const elapsed = Math.min(now - (previousTime || now), 32) / 16;
      previousTime = now;
      let hit = document.elementFromPoint(point.x, point.y);
      while (hit) {
        if (!(hit instanceof HTMLElement)) { hit = hit.parentElement; continue; }
        const style = getComputedStyle(hit);
        const box = hit.getBoundingClientRect();
        const speed = (value: number, min: number, max: number) =>
          value < min + 28 ? -14 * Math.min(1, (min + 28 - value) / 28)
            : value > max - 28 ? 14 * Math.min(1, (value - max + 28) / 28) : 0;
        const x = /auto|scroll/.test(style.overflowX) && hit.scrollWidth > hit.clientWidth
          ? speed(point.x, box.left, box.right) * elapsed : 0;
        const y = /auto|scroll/.test(style.overflowY) && hit.scrollHeight > hit.clientHeight
          ? speed(point.y, box.top, box.bottom) * elapsed : 0;
        if (x || y) {
          hit.scrollBy(x, y);
          break;
        }
        hit = hit.parentElement;
      }
      // Live session updates can move a target even while the pointer rests.
      callbacks.hover(point);
      frame = requestAnimationFrame(scroll);
    };
    const move = (next: PointerEvent) => {
      if (next.pointerId !== event.pointerId) return;
      if (!element.isConnected) { finish(); return; }
      // A cursor refresh may deliver an unrelated hover move during capture.
      // Only held-button moves update the gesture; up/cancel/capture loss end it.
      if (!(next.buttons & 1)) return;
      point = { x: next.clientX, y: next.clientY };
      if (!active) {
        if (Math.hypot(point.x - origin.x, point.y - origin.y) < 6) return;
        active = true;
        start();
        try { element.setPointerCapture(event.pointerId); }
        catch { finish(); return; }
        document.documentElement.setAttribute("data-order-dragging", "true");
        preview = document.createElement("div");
        preview.className = "order-drag-preview";
        preview.setAttribute("aria-hidden", "true");
        // The row's title alone, without the age or labels beside it.
        preview.textContent = (handle.querySelector<HTMLElement>("[data-order-title]") ?? handle).innerText.trim();
        document.body.append(preview);
        frame = requestAnimationFrame(scroll);
      }
      next.preventDefault();
      position();
    };
    // A completed drag must not also activate/collapse/archive the source.
    // A fresh pointer press removes the guard, so the next real click works.
    const suppressClick = () => {
      const remove = () => {
        window.removeEventListener("click", click, true);
        window.removeEventListener("pointerdown", remove, true);
      };
      const click = (click: MouseEvent) => {
        click.preventDefault();
        click.stopImmediatePropagation();
        remove();
      };
      window.addEventListener("click", click, true);
      window.addEventListener("pointerdown", remove, true);
      setTimeout(remove, 500);
    };
    const up = (next: PointerEvent) => {
      if (next.pointerId !== event.pointerId) return;
      const commit = active ? callbacks.drop({ x: next.clientX, y: next.clientY }) : undefined;
      if (active) suppressClick();
      finish();
      commit?.();
    };
    const key = (next: KeyboardEvent) => {
      if (next.key !== "Escape") return;
      if (active) {
        next.preventDefault();
        next.stopPropagation();
        suppressClick();
      }
      finish();
    };
    const lost = (next: PointerEvent) => {
      if (next.pointerId === event.pointerId) {
        if (active) suppressClick();
        finish();
      }
    };
    function finish() {
      if (cancel !== finish) return;
      cancel = undefined;
      cancelAnimationFrame(frame);
      window.removeEventListener("pointermove", move, true);
      window.removeEventListener("pointerup", up, true);
      window.removeEventListener("pointercancel", lost, true);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("blur", finish);
      element.removeEventListener("lostpointercapture", lost);
      if (element.hasPointerCapture(event.pointerId)) element.releasePointerCapture(event.pointerId);
      preview?.remove();
      document.documentElement.removeAttribute("data-order-dragging");
      callbacks.clear();
    }
    cancel = finish;
    window.addEventListener("pointermove", move, { capture: true, passive: false });
    window.addEventListener("pointerup", up, true);
    window.addEventListener("pointercancel", lost, true);
    window.addEventListener("keydown", key, true);
    window.addEventListener("blur", finish);
    element.addEventListener("lostpointercapture", lost);
  }
  return { begin };
}
