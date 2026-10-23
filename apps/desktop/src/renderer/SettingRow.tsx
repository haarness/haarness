import { createSignal, Show, type JSX } from "solid-js";
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
/** A folder as a person reads it: the home folder as `~`, and a long path
 * giving way in the middle so its last folder stays in sight. The whole path
 * is the tooltip and stays in the text for copying and finding. */
export function PathText(props: { path: string; class?: string }) {
  const parts = () => splitPath(props.path);
  return (
    <span class={"path-text " + (props.class ?? "")} title={props.path} dir="ltr">
      <span class="path-text-head">{parts().head}</span>
      <span class="path-text-tail">{parts().tail}</span>
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
        {props.detail}
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
