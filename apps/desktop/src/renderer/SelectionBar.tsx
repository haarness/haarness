import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { t } from "./i18n";
import { useApp } from "./state";
import { confirm } from "./ui";
import { chatTitle } from "./chat-title";
import { dropWorktree } from "./worktrees";
import { errorText } from "./error-text";
import { overlayOpen } from "./keyboard-nav";
import { locationName } from "../projects";
import {
  drawnChats,
  pickAll,
  selection,
  setSelection,
  settle,
} from "./sidebar-selection";

/** Why one item of a bulk action failed, in the reader's words where the
 * server's reason is one Desktop knows. */
function reason(error: unknown) {
  const text = errorText(error);
  if (/local changes/i.test(text.message)) return t("uncommitted changes");
  if (/active server/i.test(text.message)) return t("still running");
  return text.known ? t(text.message) : text.message;
}

/**
 * The bar under the sidebar list while Select chats is on: how many are
 * picked, Select all shown, Archive for the chats, Remove worktrees for
 * picked worktrees, and Cancel (or Escape). Each action asks first, lists
 * what it touches, and ends with a notice that names anything it could not
 * do; what it handled leaves the selection.
 */
export function SelectionBar() {
  const app = useApp();
  const [busy, setBusy] = createSignal(false);
  /** The list's chat rows that still exist, not archived: what the mode can
   * act on. */
  const live = () => {
    const current = selection();
    if (!current) return [];
    return app
      .conversations(current.scope === "all" ? undefined : current.scope)
      .filter(({ thread }) => !thread.archived && !thread.isSubagent);
  };
  const rows = () => {
    const list = live();
    return (selection()?.chats ?? []).flatMap((key) => list.find((row) => row.key === key) ?? []);
  };
  const project = () => app.projects().find((item) => item.id === selection()?.scope);
  const trees = () =>
    project()?.locations.filter((location) => selection()?.trees.includes(location.id)) ?? [];
  // Picks that no longer exist count for nothing.
  const count = () => rows().length + trees().length;
  const stop = () => setSelection(null);
  /** Leaves the picks an action handled, and any that no longer exist. */
  const settleAfter = (done: { chats?: string[]; trees?: string[] }) =>
    setSelection(
      (current) =>
        current &&
        settle(current, done, {
          chats: live().map((row) => row.key),
          trees: project()?.locations.map((location) => location.id) ?? [],
        }),
    );
  // The mode belongs to one list: it ends when that list goes away, as when
  // its project leaves the sidebar or the sidebar changes how it is organized.
  createEffect(() => {
    const current = selection();
    if (!current) return;
    const flat = app.state.sidebarLayout === "flat";
    if (current.scope === "all" ? !flat : flat || !project()) stop();
  });
  /** The result of one action: what it did, then anything it could not do. */
  function report(done: string, failed: string[]) {
    app.notify(
      failed.length
        ? done + " " + t("{count} failed: {items}", { count: failed.length, items: failed.join("; ") })
        : done,
      failed.length ? "blocked" : "done",
    );
  }
  async function archive() {
    const picked = rows();
    const working = picked.some((row) => row.running);
    const confirmed = await confirm(
      picked.length === 1
        ? t("Archive 1 chat?")
        : t("Archive {count} chats?", { count: picked.length }),
      t("Archived chats leave the sidebar. Search still finds them, and each one can be restored.") +
        (working ? " " + t("Archiving does not stop a chat that is still working.") : ""),
      {
        submit: t("Archive"),
        list: picked.map((row) => ({
          text: chatTitle(row.thread.title),
          tag: row.running ? t("Working") : undefined,
        })),
      },
    );
    if (!confirmed) return;
    setBusy(true);
    const failed: string[] = [];
    let done = 0;
    try {
      for (const row of picked) {
        try {
          await window.haarness.api(row.location.id, "updateThread", [
            row.thread.id,
            { archived: true },
          ]);
          done++;
        } catch (error) {
          failed.push(t("{name}: {reason}", { name: chatTitle(row.thread.title), reason: reason(error) }));
        }
      }
      if (picked.some((row) => row.location.id === app.state.connection?.id))
        await app.refresh();
    } finally {
      setBusy(false);
    }
    settleAfter({ chats: picked.map((row) => row.key) });
    report(done === 1 ? t("Archived 1 chat") : t("Archived {count} chats", { count: done }), failed);
  }
  async function remove() {
    const group = project();
    const picked = trees();
    if (!group || !picked.length) return;
    const running = (id: string) =>
      app.conversations(group.id).some((row) => row.location.id === id && row.running);
    const confirmed = await confirm(
      picked.length === 1
        ? t("Remove 1 worktree?")
        : t("Remove {count} worktrees?", { count: picked.length }),
      t("Their folders are deleted, and their chats leave the sidebar. A worktree with uncommitted changes or work still running is kept."),
      {
        submit: t("Remove worktrees"),
        danger: true,
        list: picked.map((location) => ({
          text: t(locationName(location)),
          tag: running(location.id) ? t("Working") : undefined,
        })),
      },
    );
    if (!confirmed) return;
    setBusy(true);
    const failed: string[] = [];
    let done = 0;
    try {
      for (const location of picked) {
        try {
          if (await dropWorktree(app, group, location)) done++;
        } catch (error) {
          failed.push(t("{name}: {reason}", { name: t(locationName(location)), reason: reason(error) }));
        }
      }
    } finally {
      setBusy(false);
    }
    settleAfter({ trees: picked.map((item) => item.id) });
    report(done === 1 ? t("Removed 1 worktree") : t("Removed {count} worktrees", { count: done }), failed);
  }
  onMount(() => {
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || !selection() || busy()) return;
      if (overlayOpen(document)) return;
      event.preventDefault();
      stop();
    };
    window.addEventListener("keydown", escape);
    onCleanup(() => window.removeEventListener("keydown", escape));
  });
  return (
    <Show when={selection()}>
      {(current) => (
        <div class="selection-bar" role="toolbar" aria-label={t("Chat selection")}>
          <div class="selection-bar-row">
            <span class="selection-count" role="status">
              {t("{count} selected", { count: count() })}
            </span>
            <button
              class="small"
              disabled={busy()}
              onClick={() => setSelection(pickAll(current(), drawnChats(current().scope)))}
            >
              {t("Select all shown")}
            </button>
          </div>
          <div class="selection-bar-row">
            <button
              class="primary"
              disabled={busy() || !rows().length}
              onClick={() => void app.run(archive)}
            >
              {t("Archive")}
            </button>
            <Show when={trees().length}>
              <button class="danger" disabled={busy()} onClick={() => void app.run(remove)}>
                {t("Remove worktrees")}
              </button>
            </Show>
            <button disabled={busy()} onClick={stop}>
              {t("Cancel")}
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
