import { createEffect, createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import { t } from "./i18n";
import { Icon } from "./icons";
import { splitPath } from "./settings-lists";
/** Roughly three lines at the description's measure; past that a row would
 * push its own control out of sight, so the text clamps until asked. */
const CLAMP_OVER = 220;
/** The quiet tag on an item that comes from the base configuration folder
 * (`HAARNESS_BASE_CONFIG_DIR`), which Haarness lists but never changes: an
 * edit saves the person's own version, so such an item has no Remove. */
export function BaseTag() {
  return (
    <span class="badge setting-tag" title={t("From the base folder")}>
      {t("Base")}
    </span>
  );
}
/** The server or computer this window shows, marked beside its name rather
 * than with a button that does nothing. */
export function CurrentTag() {
  return (
    <span class="badge setting-tag current-tag">
      <Icon name="check" size="xs" />
      {t("Current")}
    </span>
  );
}
/** This computer's home folder, read once from Electron. */
let homeFolder: Promise<string> | undefined;
const [home, setHome] = createSignal("");
function localHome() {
  homeFolder ??= window.haarness
    .boot()
    .then((boot) => boot.home ?? "")
    .catch(() => "");
  void homeFolder.then(setHome);
  return home();
}
let measure: CanvasRenderingContext2D | null | undefined;
/**
 * A folder as a person reads it: this computer's home folder as `~` (`local`,
 * a path of the server Desktop runs on), and a long path giving way in the
 * middle so its last folder stays in sight. The cut is measured, so the
 * ellipsis meets the last folder without a gap. The whole path is the
 * tooltip and what assistive technology reads.
 */
export function PathText(props: { path: string; local?: boolean; class?: string }) {
  let element!: HTMLSpanElement;
  const parts = () => splitPath(props.path, props.local ? localHome() : undefined);
  const [head, setHead] = createSignal<string>();
  function fit() {
    const box = element?.parentElement;
    const { head: full, tail } = parts();
    measure ??= document.createElement("canvas").getContext("2d");
    if (!box || !measure || !full) return setHead(undefined);
    const style = getComputedStyle(element);
    const space = getComputedStyle(box);
    const width = box.clientWidth - parseFloat(space.paddingLeft) - parseFloat(space.paddingRight);
    measure.font = style.font;
    measure.letterSpacing = style.letterSpacing === "normal" ? "0px" : style.letterSpacing;
    const fits = (text: string) => measure!.measureText(text + tail).width <= width;
    if (fits(full)) return setHead(full);
    let low = 0;
    let high = full.length;
    while (low < high) {
      const middle = Math.ceil((low + high) / 2);
      if (fits(full.slice(0, middle) + "…")) low = middle;
      else high = middle - 1;
    }
    setHead(full.slice(0, low).replace(/[/\\\s]+$/, "") + "…");
  }
  onMount(() => {
    const observer = new ResizeObserver(() => fit());
    if (element.parentElement) observer.observe(element.parentElement);
    onCleanup(() => observer.disconnect());
    void document.fonts?.ready.then(() => fit());
  });
  createEffect(() => {
    parts();
    queueMicrotask(fit);
  });
  return (
    <span ref={element} class={"path-text " + (props.class ?? "")} title={props.path} dir="ltr">
      <span class="sr-only">{props.path}</span>
      <span class="path-text-head" aria-hidden="true">{head() ?? parts().head}</span>
      <span class="path-text-tail" aria-hidden="true">{parts().tail}</span>
    </span>
  );
}
export function SettingRow(props: {
  title: string;
  target?: string;
  description?: string;
  /** A tag beside the title, such as {@link BaseTag}. */
  tag?: JSX.Element;
  /** Content under the title that is not a sentence, such as a path. */
  detail?: JSX.Element;
  /** The title and description are names and text from elsewhere (a tool's
   * own), shown as they are rather than looked up in the catalog. */
  literal?: boolean;
  children: JSX.Element;
}) {
  const [expanded, setExpanded] = createSignal(false);
  // Text from elsewhere keeps its own direction (an English description in
  // an Arabic page keeps its full stop at its end) inside the page's row.
  const text = (value: string) => (props.literal ? <span dir="auto">{value}</span> : t(value));
  const description = () => (!props.description ? "" : props.literal ? props.description : t(props.description));
  const long = () => description().length > CLAMP_OVER;
  return (
    <div class="setting-row" data-setting={props.target ?? props.title}>
      <div>
        <strong>{text(props.title)}</strong>
        {props.tag}
        {props.detail}
        <Show when={props.description}>
          <p classList={{ clamped: long() && !expanded() }}>{props.literal ? <span dir="auto">{description()}</span> : description()}</p>
        </Show>
        <Show when={long()}>
          <button
            type="button"
            class="setting-row-more"
            aria-expanded={expanded()}
            onClick={() => setExpanded((value) => !value)}
          >
            {expanded() ? t("Show less") : t("Show more")}
          </button>
        </Show>
      </div>
      {props.children}
    </div>
  );
}
