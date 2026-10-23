import { t } from "./i18n";
import { For, Show, createEffect, createSignal, onCleanup, untrack } from "solid-js";
import { useApp } from "./state";

type Delivery = "steer" | "queue";

interface InputSettingsValue {
  delivery: Delivery;
  scopeDelivery: Delivery | null;
  inheritedDelivery: Delivery;
  source: string;
  revision: string;
  message?: string;
}

/**
 * The setting belongs to the connected server/project, not Electron storage.
 *
 * One choice shows what a message sent during work does here. Without a
 * project override it is the global setting, and choosing changes it for
 * every project; "Override for this project" saves this project's own value
 * (the same one, to start with), and "Use global setting" removes it again.
 * A value a parent folder sets is changed with an override of this project's,
 * since the global setting would not reach it.
 */
export function InputSettings() {
  const app = useApp();
  const projectConfig = () => app.state.connection?.info.projectTrusted !== false;
  const [value, setValue] = createSignal<InputSettingsValue>();
  const [pending, setPending] = createSignal<Delivery>();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const supported = () => app.state.connection?.info.capabilities.includes("input-settings.v1");
  // Read where this project stands; with project configuration off, only the
  // global setting applies.
  const readScope = () => (projectConfig() ? "project" : "global");
  const overridden = () => projectConfig() && value()?.scopeDelivery != null;
  const fromParent = () => projectConfig() && !overridden() && value()?.source === "project";
  const writeScope = () => (overridden() || fromParent() ? "project" : "global");
  const shown = () => pending() ?? value()?.delivery;
  let generation = 0;
  /** `force` runs even while a read is under way (a save, or a new
   * connection); `clear` forgets the value shown, which belongs to another
   * connection. */
  async function request(input: Record<string, unknown> = { action: "get" }, { force = false, clear = false } = {}) {
    const connection = app.state.connection?.id;
    if (!connection || !supported() || (busy() && !force)) return;
    const current = ++generation;
    setBusy(true);
    if (clear) setValue(undefined);
    try {
      const call = (body: Record<string, unknown>) =>
        window.haarness.api<{ data: InputSettingsValue }>(connection, "manageResource", ["input-settings", body]);
      // A change is read back where the row reads, which may not be the scope
      // written; it carries the revision read last, so a stale save is refused.
      if (input.action !== "get") await call({ ...input, revision: value()?.revision });
      const result = await call({ action: "get", scope: readScope() });
      if (current !== generation || connection !== app.state.connection?.id) return;
      setValue(result.data);
      setError("");
    } catch (cause) {
      if (current === generation) setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (current === generation) setBusy(false);
    }
  }
  // Saves run one after another, each with the revision the one before read
  // back; the choice shows at once and until the last of them is done.
  let saves = Promise.resolve();
  let waiting = 0;
  function save(scope: string, delivery: Delivery | "inherit") {
    waiting++;
    saves = saves
      .then(() => request({ action: "set", scope, delivery }, { force: true }))
      .finally(() => {
        if (--waiting === 0) setPending(undefined);
      });
  }
  function choose(delivery: Delivery) {
    if (!value() || delivery === shown()) return;
    setPending(delivery);
    save(writeScope(), delivery);
  }
  createEffect(() => {
    void app.state.connection?.id;
    void readScope();
    untrack(() => void request({ action: "get" }, { force: true, clear: true }));
  });
  // This pane also reflects edits from an attached TUI or another server daemon.
  const timer = setInterval(() => void request(), 2000);
  onCleanup(() => { generation++; clearInterval(timer); });
  const options = [
    { value: "steer", label: "Steer" },
    { value: "queue", label: "Queue" },
  ] as const;
  let group!: HTMLDivElement;
  return (
    <div class="setting-row input-settings-row" data-setting="Messages sent while working">
      <div>
        <strong>{t("Messages sent while working")}</strong>
        <p>{t("Steer joins the next agent step. Queue waits until current work finishes.")}</p>
        <Show when={!supported()}><p>{t("Open a project on a server that supports input settings.")}</p></Show>
        <Show when={error()}><p role="alert">{error()}</p></Show>
      </div>
      <div class="input-settings-controls">
        <div
          ref={group}
          class="segmented"
          role="radiogroup"
          aria-label={t("Messages sent while working")}
          onKeyDown={(event) => {
            const forward = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
            if (!forward) return;
            event.preventDefault();
            // Left and right follow the reading direction, which flips right to left.
            const flip = event.key.startsWith("ArrowL") || event.key.startsWith("ArrowR")
              ? getComputedStyle(group).direction === "rtl" ? -1 : 1
              : 1;
            const index = Math.max(0, options.findIndex((option) => option.value === shown()));
            const next = options[(index + forward * flip + options.length) % options.length];
            choose(next.value);
            queueMicrotask(() => group.querySelector<HTMLElement>(`[data-value="${next.value}"]`)?.focus());
          }}
        >
          <For each={options}>
            {(option) => (
              <button
                type="button"
                role="radio"
                data-value={option.value}
                aria-checked={shown() === option.value}
                tabindex={shown() === option.value || (!shown() && option.value === "steer") ? 0 : -1}
                disabled={!supported()}
                onClick={() => choose(option.value)}
              >
                {t(option.label)}
              </button>
            )}
          </For>
        </div>
        <Show when={projectConfig() && value()}>
          <span class="input-settings-scope">
            <Show
              when={overridden()}
              fallback={
                <Show
                  when={fromParent()}
                  fallback={
                    <button type="button" class="settings-link" onClick={() => save("project", value()!.delivery)}>
                      {t("Override for this project")}
                    </button>
                  }
                >
                  {t("Set in a parent folder")}
                </Show>
              }
            >
              <span>{t("This project")}</span>
              <span aria-hidden="true">·</span>
              <button type="button" class="settings-link" onClick={() => save("project", "inherit")}>
                {t("Use global setting")}
              </button>
            </Show>
          </span>
        </Show>
      </div>
    </div>
  );
}
