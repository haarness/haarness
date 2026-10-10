import {
  Show,
  createEffect,
  createMemo,
  createSignal,
  on,
  onMount,
} from "solid-js";
import { t } from "./i18n";
import { SettingRow as Row } from "./SettingRow";
import { useApp } from "./state";
import { Button, dismissOnBackdrop } from "./ui";
import "./usage-statistics.css";

/** `manageResource("usage-statistics", …)` on the connected server. */
interface UsageStatus {
  setting: {
    enabled: boolean;
    source: "default" | "settings" | "environment" | "doNotTrack";
    editable: boolean;
  };
  lastSentAt: string | null;
  environmentVariable: string;
  report?: unknown;
}

const CAPABILITY = "usage-statistics.v1";
const TITLE = "Share usage statistics";
const DESCRIPTION =
  "Daily counts of chats, turns, tokens, models and tool calls, linked to your Haarness account when you're signed in. Never prompts, code, file names or paths.";

async function request(connection: string, input: Record<string, unknown>) {
  const result = await window.haarness.api<{ data: UsageStatus }>(
    connection,
    "manageResource",
    ["usage-statistics", input],
  );
  return result.data;
}

export function UsageStatisticsSetting() {
  const app = useApp();
  const [status, setStatus] = createSignal<UsageStatus>();
  const [previewing, setPreviewing] = createSignal(false);
  const supported = createMemo(() =>
    app.state.connection?.info.capabilities.includes(CAPABILITY),
  );
  createEffect(
    on(
      () => app.state.connection?.id,
      (connection) => {
        setStatus(undefined);
        if (!connection || !supported()) return;
        void app.run(async () => setStatus(await request(connection, { action: "get" })));
      },
    ),
  );
  const decidedBy = () => {
    const setting = status()?.setting;
    if (setting?.source === "doNotTrack")
      return t("Off because DO_NOT_TRACK is set where Haarness started.");
    if (setting?.source === "environment")
      return t("Set by {variable} where Haarness started.", {
        variable: status()!.environmentVariable,
      });
    return "";
  };
  return (
    <Show when={status()}>
      {(current) => (
        <>
          <Row title={TITLE} description={DESCRIPTION}>
            <button type="button" onClick={() => setPreviewing(true)}>
              {t("See what's shared")}
            </button>
            <input
              type="checkbox"
              role="switch"
              aria-label={t(TITLE)}
              checked={current().setting.enabled}
              disabled={!current().setting.editable}
              onChange={(event) => {
                const enabled = event.currentTarget.checked;
                const connection = app.state.connection?.id;
                if (!connection) return;
                void app.run(async () =>
                  setStatus(await request(connection, { action: "set", enabled })),
                );
              }}
            />
          </Row>
          <Show when={decidedBy()}>
            <p class="muted usage-setting-hint">{decidedBy()}</p>
          </Show>
          <Show when={previewing()}>
            <UsagePreview close={() => setPreviewing(false)} />
          </Show>
        </>
      )}
    </Show>
  );
}

function UsagePreview(props: { close: () => void }) {
  const app = useApp();
  const [report, setReport] = createSignal("");
  const [empty, setEmpty] = createSignal(false);
  const [error, setError] = createSignal("");
  let dialog!: HTMLDialogElement;
  const close = () => {
    if (dialog.open) dialog.close();
    props.close();
  };
  onMount(async () => {
    dialog.showModal();
    const connection = app.state.connection?.id;
    if (!connection) return;
    try {
      const status = await request(connection, { action: "preview" });
      const days = (status.report as { days?: unknown[] } | undefined)?.days;
      setEmpty(!days?.length);
      setReport(JSON.stringify(status.report, null, 2));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  });
  return (
    <dialog
      ref={dialog}
      class="dialog editor-dialog usage-preview-dialog"
      aria-label={t("What Haarness shares")}
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
      {...dismissOnBackdrop(close)}
    >
      <header>
        <div>
          <h2>{t("What Haarness shares")}</h2>
          <p class="muted">
            {t("The report exactly as it would be sent now: one snapshot per day for the last 7 days. Each replaces the one sent before for the same day.")}
          </p>
        </div>
        <Button icon="close" label={t("Close dialog")} onClick={close} />
      </header>
      <Show when={error()}>
        <p class="form-error" role="alert">
          {error()}
        </p>
      </Show>
      <Show
        when={report()}
        fallback={
          <Show when={!error()}>
            <p class="muted" aria-live="polite">
              {t("Checking…")}
            </p>
          </Show>
        }
      >
        <Show when={empty()}>
          <p class="muted usage-preview-empty">
            {t("No activity in the last 7 days yet, so there is nothing to send.")}
          </p>
        </Show>
        <pre class="usage-preview" tabindex="0">
          {report()}
        </pre>
      </Show>
    </dialog>
  );
}
