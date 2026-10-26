import { t, language } from "./i18n";
import { For, Match, Show, Switch, createSignal, onMount } from "solid-js";
import { Button, Icon, dismissOnBackdrop } from "./ui";
import { relativeTime } from "./relative-time";
import { toggleSelection } from "./screenshot-picker";
import type { Attachment } from "../attachments";
import type { ScreenshotListing } from "../shared";

type Listing = Extract<ScreenshotListing, { listing: string }>;

/**
 * The Screenshots dialog: the screenshots folder's newest images, newest
 * first. Clicked screenshots are numbered and go into the message in that
 * order. The folder button picks from another folder this once (on Linux,
 * where there is no screenshots folder, the chosen folder is kept).
 */
export function ScreenshotPicker(props: {
  /** From the Attach menu the dialog was chosen in; main lists the folder for it once. */
  token: Promise<string | null>;
  close: (values?: Attachment[]) => void;
}) {
  const [listing, setListing] = createSignal<Listing>();
  const [needsFolder, setNeedsFolder] = createSignal(false);
  const [loading, setLoading] = createSignal(true);
  const [adding, setAdding] = createSignal(false);
  const [error, setError] = createSignal("");
  const [selected, setSelected] = createSignal<number[]>([]);
  const now = Date.now();
  let dialog!: HTMLDialogElement;
  let grid: HTMLDivElement | undefined;
  let folderButton!: HTMLButtonElement;

  const close = (values?: Attachment[]) => {
    if (dialog.open) dialog.close();
    props.close(values);
  };
  async function load(choose = false) {
    setError("");
    if (!choose) setLoading(true);
    try {
      const token = choose ? null : await props.token;
      const value = await window.haarness.screenshots(choose ? { choose } : token ? { token } : {});
      if ("canceled" in value) {
        // Declining to show the screenshots closes the dialog; declining another folder keeps it.
        if (!choose) close();
        return;
      }
      setSelected([]);
      setNeedsFolder("needsFolder" in value);
      setListing("listing" in value ? value : undefined);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoading(false);
      queueMicrotask(() =>
        (grid?.querySelector<HTMLButtonElement>(".screenshot-tile") ?? folderButton)?.focus(),
      );
    }
  }
  async function add() {
    const value = listing();
    if (!value || !selected().length || adding()) return;
    setAdding(true);
    setError("");
    try {
      close(await window.haarness.screenshotAttach(value.listing, selected()));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setAdding(false);
    }
  }
  /** Arrow keys move between tiles, a row at a time up and down. */
  function move(event: KeyboardEvent) {
    const steps: Record<string, number> = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: 0, ArrowDown: 0 };
    if (!(event.key in steps) || !grid) return;
    const tiles = Array.from(grid.querySelectorAll<HTMLButtonElement>(".screenshot-tile"));
    const current = tiles.indexOf(document.activeElement as HTMLButtonElement);
    if (current < 0) return;
    const columns = getComputedStyle(grid).gridTemplateColumns.split(" ").filter(Boolean).length || 1;
    const rtl = getComputedStyle(grid).direction === "rtl";
    const step =
      event.key === "ArrowUp"
        ? -columns
        : event.key === "ArrowDown"
          ? columns
          : steps[event.key] * (rtl ? -1 : 1);
    const next = tiles[current + step];
    if (!next) return;
    event.preventDefault();
    next.focus();
  }
  const count = () => selected().length;
  onMount(() => {
    dialog.showModal();
    void load();
  });
  return (
    <dialog
      ref={dialog}
      class="dialog screenshot-dialog"
      aria-label={t("Screenshots")}
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
      {...dismissOnBackdrop(() => close())}
    >
      <header>
        <div>
          <h2>{t("Screenshots")}</h2>
          <Show when={listing()}>
            <p class="muted screenshot-folder-name" title={listing()!.folder}>
              {listing()!.folder}
            </p>
          </Show>
        </div>
        <Button icon="close" label={t("Close dialog")} onClick={() => close()} />
      </header>
      <Switch>
        <Match when={loading()}>
          <p class="muted screenshot-empty" role="status">
            {t("Loading…")}
          </p>
        </Match>
        <Match when={needsFolder()}>
          <p class="screenshot-empty">{t("Choose the folder your screenshots are saved in.")}</p>
        </Match>
        <Match when={listing() && !listing()!.items.length}>
          <p class="muted screenshot-empty">{t("No screenshots in this folder yet.")}</p>
        </Match>
        <Match when={listing()}>
          <div class="screenshot-grid" ref={grid} role="group" aria-label={t("Screenshots")} onKeyDown={move}>
            <For each={listing()!.items}>
              {(item, index) => {
                const order = () => selected().indexOf(index()) + 1;
                const ago = relativeTime(item.modified, now, language());
                return (
                  <button
                    type="button"
                    class="screenshot-tile"
                    aria-pressed={order() > 0}
                    aria-label={item.name}
                    aria-description={ago}
                    title={item.name}
                    onClick={() => setSelected((values) => toggleSelection(values, index()))}
                  >
                    <span class="screenshot-thumb">
                      <Show when={item.thumbnail} fallback={<Icon name="image" size="xl" />}>
                        <img src={item.thumbnail} alt="" draggable={false} />
                      </Show>
                      <Show when={order()}>
                        <span class="screenshot-order" aria-hidden="true">
                          {order()}
                        </span>
                      </Show>
                    </span>
                    <span class="screenshot-name">{item.name}</span>
                    <span class="screenshot-time">{ago}</span>
                  </button>
                );
              }}
            </For>
          </div>
        </Match>
      </Switch>
      <Show when={error()}>
        <p class="inline-error screenshot-error" role="alert">
          <Icon name="error" size="sm" />
          <span>{error()}</span>
        </p>
      </Show>
      <footer>
        <button
          ref={folderButton}
          type="button"
          class="screenshot-choose"
          onClick={() => void load(true)}
        >
          <Icon name="folder-open" size="sm" />
          {t("Choose folder…")}
        </button>
        <button type="button" onClick={() => close()}>
          {t("Cancel")}
        </button>
        <button type="button" class="primary" disabled={!count() || adding()} onClick={() => void add()}>
          {count() > 1
            ? t("Add {count} screenshots", { count: count() })
            : count() === 1
              ? t("Add 1 screenshot")
              : t("Add screenshots")}
        </button>
      </footer>
    </dialog>
  );
}
