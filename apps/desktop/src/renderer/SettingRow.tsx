import { createSignal, Show, type JSX } from "solid-js";
import { t } from "./i18n";
/** Roughly three lines at the description's measure; past that a row would
 * push its own control out of sight, so the text clamps until asked. */
const CLAMP_OVER = 220;
export function SettingRow(props: {
  title: string;
  target?: string;
  description?: string;
  children: JSX.Element;
}) {
  const [expanded, setExpanded] = createSignal(false);
  const description = () => (props.description ? t(props.description) : "");
  const long = () => description().length > CLAMP_OVER;
  return (
    <div class="setting-row" data-setting={props.target ?? props.title}>
      <div>
        <strong>{t(props.title)}</strong>
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
