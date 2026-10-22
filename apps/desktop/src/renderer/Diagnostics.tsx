import { createSignal, onCleanup, onMount } from "solid-js";
import { minutesLeft, type LogLevelStatus } from "../diagnostics";
import { formatNumber, t } from "./i18n";
import { SettingRow as Row } from "./SettingRow";
import { useApp } from "./state";

/**
 * Saves this computer's logs where the person chooses and shows the zip in
 * the file manager, which says it worked; Help → Export logs…, the palette
 * and Settings all come here.
 */
export function exportLogs() {
  return window.haarness.exportLogs();
}

/**
 * Settings → General → Diagnostics: the logs to share when something goes
 * wrong, and Detailed logging, which turns itself off after 30 minutes and
 * says how long it has left. A level raised from the CLI or the TUI shows
 * here too.
 */
export function DiagnosticsSetting() {
  const app = useApp();
  const [status, setStatus] = createSignal<LogLevelStatus>();
  const [now, setNow] = createSignal(Date.now());
  const [busy, setBusy] = createSignal(false);
  const load = () =>
    window.haarness
      .logLevel("status")
      .then(setStatus)
      .catch(() => setStatus(undefined));
  const on = () => {
    const current = status();
    return !!current?.level && (current.untilStopped || minutesLeft(current.until, now()) > 0);
  };
  onMount(() => {
    void load();
    // The time left ticks down; a level changed elsewhere shows within a minute.
    let ticks = 0;
    const timer = setInterval(() => {
      setNow(Date.now());
      if (++ticks % 4 === 0 || (status()?.level && !on())) void load();
    }, 15_000);
    onCleanup(() => clearInterval(timer));
  });
  const description = () => {
    const current = status();
    if (!on() || !current)
      return t("Records more detail in Haarness's logs for 30 minutes, then turns itself off.");
    if (current.untilStopped) return t("On until stopped");
    const minutes = minutesLeft(current.until, now());
    return minutes <= 1
      ? t("On · turns off in under a minute")
      : t("On · turns off in {minutes} min", { minutes: formatNumber(minutes) });
  };
  const change = (enabled: boolean) =>
    app.run(async () => {
      setBusy(true);
      try {
        setStatus(await window.haarness.logLevel(enabled ? "detail-on" : "detail-off"));
        setNow(Date.now());
      } finally {
        setBusy(false);
      }
    });
  return (
    <>
      <Row
        title="Diagnostics"
        description="This computer's Haarness logs as a zip, with known secrets removed, to share when something goes wrong."
      >
        <button type="button" onClick={() => app.run(exportLogs)}>
          {t("Export logs…")}
        </button>
      </Row>
      <Row title="Detailed logging" description={description()}>
        <input
          type="checkbox"
          role="switch"
          aria-label={t("Detailed logging")}
          checked={on()}
          disabled={busy() || !status()}
          onChange={(event) => {
            const enabled = event.currentTarget.checked;
            // The switch shows what is in force, not what was clicked.
            event.currentTarget.checked = on();
            void change(enabled);
          }}
        />
      </Row>
    </>
  );
}
