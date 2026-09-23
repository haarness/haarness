import { t } from "./i18n";
import { createSignal, onMount } from "solid-js";
import { ManagementPanel } from "./ManagementPanel";
import { Button, dismissOnBackdrop } from "./ui";
import { useApp } from "./state";

/** Uses the same server-owned connection wizard as attached and local terminals. */
export function RouterConnectionDialog(props: { close: () => void }) {
  const app = useApp();
  const [busy, setBusy] = createSignal(false);
  let dialog!: HTMLDialogElement;
  onMount(() => dialog.showModal());
  const close = () => {
    if (!dialog.isConnected || !dialog.open) return;
    dialog.close();
    props.close();
  };
  return (
    <dialog ref={dialog} class="dialog editor-dialog router-connection-dialog" aria-label={t("Connect Haarness Router")}
      onCancel={(event) => { event.preventDefault(); close(); }}
      {...dismissOnBackdrop(close)}>
      <header>
        <div>
          <h2>{t("Haarness Router")}</h2>
          <p class="muted">{t("Connect your self-hosted router to this Haarness server.")}</p>
        </div>
        <Button icon="close" label={t("Close dialog")} onClick={close} />
      </header>
      <ManagementPanel resource="providers" initialRequest={{ action: "router_form" }} pending={setBusy} />
      <footer>
        <button class="secondary" disabled={busy()} onClick={() => { void app.run(() => app.catalogs(true)); close(); }}>{t("Done")}</button>
      </footer>
    </dialog>
  );
}
