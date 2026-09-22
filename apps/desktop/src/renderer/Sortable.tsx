import { t } from "./i18n";
import { createSignal } from "solid-js";
import { useApp } from "./state";
import { createPointerOrdering, type OrderPoint } from "./pointer-ordering";

type Source = { scope: string; id: string };
type DropSite = {
  accepts: (source: Source) => boolean;
  hover: (point: OrderPoint, source: Source) => void;
  drop: (point: OrderPoint, source: Source) => (() => void) | undefined;
};
const [drag, setDrag] = createSignal<Source>();
const [target, setTarget] = createSignal<(Source & { after: boolean }) | undefined>(undefined, {
  equals: (a, b) => a?.scope === b?.scope && a?.id === b?.id && a?.after === b?.after,
});
const sites = new WeakMap<HTMLElement, DropSite>();

function siteAt(point: OrderPoint, source: Source) {
  // Pointer capture keeps delivery on the source; hit testing finds the actual
  // destination, including SVGs, title text, padding and nested session rows.
  for (let element = document.elementFromPoint(point.x, point.y); element; element = element.parentElement) {
    const site = sites.get(element as HTMLElement);
    if (site?.accepts(source)) return site;
  }
}
const pointer = createPointerOrdering({
  hover(point) {
    const source = drag();
    const site = source && siteAt(point, source);
    if (site) site.hover(point, source!);
    else setTarget(undefined);
  },
  drop(point) {
    const source = drag();
    return source && siteAt(point, source)?.drop(point, source);
  },
  clear() { setDrag(undefined); setTarget(undefined); },
});

/** A collection's heading and background accept its in-app project drag,
 * including when empty. External text/files never become row IDs. */
export function createOrderDropTarget(
  scope: () => string,
  id: () => string,
  receive: (source: string) => void,
) {
  return {
    ref(element: HTMLElement) {
      sites.set(element, {
        accepts: source => source.scope === scope(),
        hover: () => setTarget({ scope: scope(), id: id(), after: false }),
        drop: (_, source) => () => receive(source.id),
      });
    },
    get "data-drop-position"() {
      return target()?.scope === scope() && target()?.id === id() ? "inside" : undefined;
    },
  };
}

export function createOrdering(
  scope: () => string,
  ids: () => string[],
  newcomers: "start" | "end" = "end",
) {
  const app = useApp();
  const ordered = () => app.ordered(scope(), ids(), newcomers);
  const move = (id: string, to: number) => {
    const values = ordered();
    const index = Math.max(0, Math.min(to, values.length - 1));
    if (values.indexOf(id) === index) return;
    void app.run(() => app.reorder(scope(), values, id, index));
  };
  const isAfter = (source: string, id: string, box: DOMRect, point: OrderPoint, axis: "x" | "y") => {
    const fraction = axis === "x" ? (point.x - box.x) / box.width : (point.y - box.y) / box.height;
    // The middle swaps neighbours; the edges select an exact insertion gap.
    if (fraction < 0.25) return false;
    if (fraction > 0.75) return true;
    const values = ordered();
    return values.indexOf(source) < values.indexOf(id);
  };
  const receive = (source: string, id: string, after: boolean) => {
    const values = ordered();
    const from = values.indexOf(source), to = values.indexOf(id);
    if (source !== id && from >= 0 && to >= 0)
      move(source, to + (after ? 1 : 0) - (from < to ? 1 : 0));
  };
  // Resolve insertion gaps and list padding to the nearest visible row.
  // A different project's conversation list never accepts this source.
  const regionProps = () => ({
    ref(element: HTMLElement) {
      const rowAt = (point: OrderPoint) => {
        const values = ordered();
        return Array.from(element.querySelectorAll<HTMLElement>("[data-sort-id]"))
          .filter(row => row.dataset.sortScope === scope() && values.includes(row.dataset.sortId!))
          .map(row => ({ id: row.dataset.sortId!, box: row.getBoundingClientRect() }))
          .filter(row => row.box.height > 0)
          .sort((a, b) => {
            const distance = (box: DOMRect) => Math.max(box.top - point.y, point.y - box.bottom, 0);
            return distance(a.box) - distance(b.box);
          })[0];
      };
      sites.set(element, {
        accepts: source => source.scope === scope() && ordered().includes(source.id),
        hover(point, source) {
          const row = rowAt(point);
          setTarget(row ? { scope: scope(), id: row.id, after: isAfter(source.id, row.id, row.box, point, "y") } : undefined);
        },
        drop(point, source) {
          const row = rowAt(point);
          if (!row) return;
          const after = isAfter(source.id, row.id, row.box, point, "y");
          return () => receive(source.id, row.id, after);
        },
      });
    },
  });
  const dropProps = (
    id: string,
    label: string,
    axis: "x" | "y" = "y",
    anchor?: string,
    foreignDrop?: {
      accepts: (source: string) => boolean;
      receive: (source: string, position: "before" | "after") => void;
    },
  ) => {
    const handle = (row: HTMLElement) =>
      (anchor && row.querySelector<HTMLElement>(anchor)) || row;
    return {
      ref(element: HTMLElement) {
        sites.set(element, {
          accepts: source => source.scope === scope() &&
            (ordered().includes(source.id) || !!foreignDrop?.accepts(source.id)),
          hover(point, source) {
            setTarget({ scope: scope(), id, after: isAfter(source.id, id, handle(element).getBoundingClientRect(), point, axis) });
          },
          drop(point, source) {
            if (source.id === id) return;
            const after = isAfter(source.id, id, handle(element).getBoundingClientRect(), point, axis);
            const local = ordered().includes(source.id);
            return () => local ? receive(source.id, id, after)
              : foreignDrop?.receive(source.id, after ? "after" : "before");
          },
        });
      },
      draggable: false,
      "data-sort-id": id,
      "data-sort-scope": scope(),
      "data-sort-axis": axis,
      "aria-description": t("Drag to reorder {name}. Alt and arrow keys also move it.", { name: label }),
      "aria-keyshortcuts": axis === "x"
        ? "Alt+ArrowLeft Alt+ArrowRight Alt+Home Alt+End"
        : "Alt+ArrowUp Alt+ArrowDown Alt+Home Alt+End",
      get "data-order-source"() {
        return drag()?.scope === scope() && drag()?.id === id ? "true" : undefined;
      },
      get "data-drop-position"() {
        const value = target();
        return value?.scope === scope() && value.id === id && drag()?.id !== id
          ? value.after ? "after" : "before" : undefined;
      },
      onPointerDown(event: PointerEvent & { currentTarget: HTMLElement }) {
        const row = event.currentTarget;
        const grip = handle(row);
        // Scheduled chats and checkout controls cannot drag their project.
        if (anchor && !grip.contains(event.target as Node)) return;
        if (event.button !== 0 || !event.isPrimary) return;
        event.stopPropagation();
        pointer.begin(event, row, grip, () => setDrag({ scope: scope(), id }));
      },
      onDragStart(event: DragEvent) {
        event.preventDefault();
        event.stopPropagation();
      },
      onKeyDown(event: KeyboardEvent & { currentTarget: HTMLElement }) {
        if (!event.altKey || event.ctrlKey || event.metaKey) return;
        if (anchor && !handle(event.currentTarget).contains(event.target as Node)) return;
        if ((event.target as HTMLElement).closest("[data-sort-id]") !== event.currentTarget) return;
        const keys = axis === "x" ? ["ArrowLeft", "ArrowRight"] : ["ArrowUp", "ArrowDown"];
        if (![...keys, "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        event.stopPropagation();
        const focused = document.activeElement as HTMLElement;
        const values = ordered();
        move(id, event.key === "Home" ? 0 : event.key === "End" ? values.length - 1
          : values.indexOf(id) + (event.key === keys[0] ? -1 : 1));
        queueMicrotask(() => {
          focused?.focus({ preventScroll: true });
          focused?.scrollIntoView({ block: "nearest", inline: "nearest" });
        });
      },
    };
  };
  return { scope, ordered, move, dropProps, regionProps };
}
