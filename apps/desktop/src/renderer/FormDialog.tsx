import { t } from "./i18n";
import { Show, createSignal, onMount, type Accessor, type JSX } from "solid-js";
import { Button, dismissOnBackdrop } from "./ui";

/** Preserve failed drafts and allow dismissal without waiting for pending writes. */
export function FormDialog(props: {
  title: string;
  description?: string;
  submit?: string;
  class?: string;
  submitDisabled?: boolean;
  pendingLabel?: string;
  pending?: boolean;
  save: () => Promise<unknown>;
  close: () => void;
  children: (busy: Accessor<boolean>) => JSX.Element;
}) {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  let dialog!: HTMLDialogElement;
  let alert!: HTMLParagraphElement;
  onMount(() => dialog.showModal());
  const locked = () => busy() || !!props.pending;
  const close = () => {
    if (!dialog.isConnected || !dialog.open) return;
    dialog.close();
    props.close();
  };
  return (
    <dialog
      ref={dialog}
      class={`dialog editor-dialog ${props.class ?? ""}`}
      aria-label={props.title}
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
      {...dismissOnBackdrop(close)}
    >
      <form
        onInput={() => setError("")}
        onChange={() => setError("")}
        onSubmit={async (event) => {
          event.preventDefault();
          if (locked() || props.submitDisabled || !props.submit) return;
          setError("");
          setBusy(true);
          try {
            await props.save();
            setBusy(false);
            close();
          } catch (error) {
            setBusy(false);
            setError(error instanceof Error ? error.message : String(error));
            alert?.focus();
          }
        }}
      >
        <header>
          <div>
            <h2>{props.title}</h2>
            <Show when={props.description}>
              <p class="muted">{props.description}</p>
            </Show>
          </div>
          <Button
            icon="close"
            label={t("Close dialog")}
            onClick={close}
          />
        </header>
        <fieldset
          disabled={locked()}
          onPointerDown={() => setError("")}
          onKeyDown={() => setError("")}
        >
          {props.children(locked)}
        </fieldset>
        <Show when={error()}>
          <p ref={alert} class="form-error" role="alert" tabindex="-1">
            {error()}
          </p>
        </Show>
        <footer>
          <button type="button" onClick={close}>
            {props.submit ? t("Cancel") : t("Close")}
          </button>
          <Show when={props.submit}>
            <button
              class="primary"
              type="submit"
              disabled={locked() || props.submitDisabled}
            >
              {busy() ? (props.pendingLabel ?? t("Saving…")) : props.submit}
            </button>
          </Show>
        </footer>
      </form>
    </dialog>
  );
}
