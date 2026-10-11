/* Controls shared by the popup and the welcome page. */
import { createSignal, onCleanup, type JSX } from "solid-js";
import { key } from "../core/i18n";
import type { Pill } from "../core/status";
import type { UiResponse } from "../core/ui";
import { Icon } from "./icons";
import { t } from "./i18n";

const pillLabels: Record<Pill, string> = {
  connected: key("Connected"),
  connecting: key("Connecting…"),
  "not-set-up": key("Not set up"),
  paused: key("Paused"),
  stopped: key("Stopped"),
  update: key("Update ready"),
  mismatch: key("Update needed"),
};

export function StatusPill(props: { pill: Pill }) {
  return (
    <span class={"pill " + props.pill} role="status" data-pill={props.pill}>
      <span class="dot" aria-hidden="true" />
      {t(pillLabels[props.pill])}
    </span>
  );
}

/** A command or path with a Copy button that confirms in place. */
export function CopyField(props: { value: string; label: string }) {
  const [copied, setCopied] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(timer));
  const copy = async () => {
    await navigator.clipboard.writeText(props.value).catch(() => {});
    setCopied(true);
    clearTimeout(timer);
    timer = setTimeout(() => setCopied(false), 1_600);
  };
  return (
    <div class="copy-field">
      <code title={props.value}>{props.value}</code>
      <button
        type="button"
        class="icon-button"
        classList={{ copied: copied() }}
        aria-label={copied() ? t("Copied") : props.label}
        title={copied() ? t("Copied") : props.label}
        onClick={copy}
      >
        <Icon name={copied() ? "check" : "copy"} size="sm" />
      </button>
    </div>
  );
}

export function Switch(props: {
  label: string;
  checked: boolean;
  disabled?: boolean;
  onChange(checked: boolean): void;
  id?: string;
}) {
  return (
    <input
      id={props.id}
      type="checkbox"
      role="switch"
      aria-label={props.label}
      checked={props.checked}
      disabled={props.disabled}
      onChange={(event) => props.onChange(event.currentTarget.checked)}
    />
  );
}

export function Spinner() {
  return <span class="spinner" aria-hidden="true" />;
}

type Failure = Extract<UiResponse, { ok: false }>;

/**
 * Actions for the rows of a list, each run with its control busy until it
 * finishes and its failure reported under it. Every snapshot gives the
 * popup's lists new rows, so this state lives with the list, by row id.
 */
export function useActions() {
  const [states, setStates] = createSignal<Record<string, { busy: boolean; failed?: Failure }>>({});
  const update = (id: string, state: { busy: boolean; failed?: Failure }) => setStates((all) => ({ ...all, [id]: state }));
  return (id: string) => ({
    busy: () => states()[id]?.busy ?? false,
    /** The failure in the page's language (translated as it renders, so it follows a language change), or "". */
    error: () => {
      const failed = states()[id]?.failed;
      return failed ? t(failed.error, failed.values) : "";
    },
    async run(work: () => Promise<UiResponse>) {
      update(id, { busy: true });
      let failed: Failure | undefined;
      try {
        const result = await work();
        if (!result.ok) failed = result.error ? result : { ok: false, error: key("Something went wrong.") };
      } finally {
        update(id, { busy: false, failed });
      }
    },
  });
}

export type Action = ReturnType<ReturnType<typeof useActions>>;

/** One action, for a control that is not in a list. */
export function useAction(): Action {
  return useActions()("");
}

export function ErrorLine(props: { children: JSX.Element }) {
  return (
    <p class="error-line" role="alert">
      <Icon name="error" size="sm" />
      <span>{props.children}</span>
    </p>
  );
}
