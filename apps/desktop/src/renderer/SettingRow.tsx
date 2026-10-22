import { createSignal, Show, type JSX } from "solid-js";
import { t } from "./i18n";
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
export function SettingRow(props: {
  title: string;
  target?: string;
  description?: string;
  /** A tag beside the title, such as {@link BaseTag}. */
  tag?: JSX.Element;
  children: JSX.Element;
}) {
  const [expanded, setExpanded] = createSignal(false);
  const description = () => (props.description ? t(props.description) : "");
  const long = () => description().length > CLAMP_OVER;
  return (
    <div class="setting-row" data-setting={props.target ?? props.title}>
      <div>
        <strong>{t(props.title)}</strong>
        {props.tag}
        <Show when={props.description}>
          <p classList={{ clamped: long() && !expanded() }}>{description()}</p>
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
