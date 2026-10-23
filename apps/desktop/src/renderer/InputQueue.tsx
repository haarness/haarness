import { t } from "./i18n";
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup } from "solid-js";
import type { EventEnvelope, InputQueue as Queue, InputQueueAction, PendingInput } from "@haarness/sdk";
import { useApp } from "./state";
import { Button, Icon } from "./ui";
import { attachmentName, type Attachment } from "./media";
import { carriedResource, sentParts, type PromptContext } from "./prompt-context";
import { pasteLabel } from "./PromptContext";
import "./input-queue.css";

/** What the composer held before it started editing a queued message. */
type Draft = { text: string; attachments: Attachment[]; context: PromptContext[] };
type Edit = { id: string; stash: Draft; original: PendingInput };

/**
 * Prompts queued behind the current work, shared by the tray above the
 * composer and the composer itself: editing loads a queued message into the
 * composer, and sending there saves it back in place.
 */
export function createInputQueue(composer: {
  owner: () => string;
  working: () => boolean;
  read: () => Draft;
  write: (draft: Draft) => void;
}) {
  const app = useApp();
  const [queue, setQueue] = createSignal<Queue>({ revision: 0, items: [] });
  const [edits, setEdits] = createSignal<Record<string, Edit>>({});
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const supported = () => app.state.connection?.info.capabilities.includes("input-queue.v1");
  const owner = () => `${app.state.connection?.id}:${app.state.selected}`;
  // A reload drops the editor; the draft set aside for it comes back.
  const stashKey = (key: string) => "queue-edit:" + key;
  let generation = 0;
  const accept = (value: Queue) => {
    if (value.revision >= queue().revision) setQueue(value);
  };
  async function refresh() {
    if (!supported() || !app.state.selected) return;
    const origin = owner(), epoch = generation;
    const value = await app.api<Queue>("inputQueue", app.state.selected);
    if (origin === owner() && epoch === generation) accept(value);
  }
  createEffect(on(owner, () => {
    generation++;
    setQueue({ revision: 0, items: [] }); setError(""); setBusy(false);
    const epoch = generation, key = composer.owner();
    void refresh().catch((reason) => {
      if (epoch === generation) setError(String(reason));
    });
    if (edits()[key]) return;
    void window.haarness.storage(stashKey(key)).then((saved) => {
      if (!saved || epoch !== generation || edits()[key]) return;
      composer.write(saved as Draft);
      void window.haarness.storage(stashKey(key), null);
    });
  }));
  createEffect(() => {
    app.state.connectionStatus;
    void refresh().catch(() => {});
  });
  const off = window.haarness.onEvent((event) => {
    if (event.connection !== app.state.connection?.id || event.kind !== "server") return;
    const data = event.data as EventEnvelope;
    if (data.type === "input.queue.updated" && data.threadId === app.state.selected)
      accept(data.payload as Queue);
  });
  onCleanup(off);
  const editing = () => edits()[composer.owner()];
  const finish = (key: string) => {
    setEdits(({ [key]: _, ...rest }) => rest);
    void window.haarness.storage(stashKey(key), null);
  };
  async function mutate(action: InputQueueAction) {
    const origin = owner(), epoch = generation;
    setBusy(true); setError("");
    try {
      const value = await app.api<Queue>("changeInputQueue", app.state.selected, action);
      if (origin === owner() && epoch === generation) accept(value);
    } catch (reason) {
      if (origin === owner() && epoch === generation) {
        setError(reason instanceof Error ? reason.message : String(reason));
        await refresh().catch(() => {});
      }
      throw reason;
    } finally {
      if (origin === owner() && epoch === generation) setBusy(false);
    }
  }
  const run = (action: InputQueueAction) => void mutate(action).catch(() => {});
  /** Load a queued message into the composer, setting the draft aside. */
  function edit(item: PendingInput) {
    if (busy()) return;
    const key = composer.owner();
    const stash = editing()?.stash ?? composer.read();
    setEdits((value) => ({ ...value, [key]: { id: item.id, stash, original: item } }));
    void window.haarness.storage(stashKey(key), stash);
    setError("");
    // Queued messages carry text and attachments, so the composer's context
    // chips are set aside with the rest of the draft; a paste the message
    // holds comes back as its chip, and a resource the server read as a chip
    // holding what was read, and both go back in place when saved.
    const parts = sentParts(item.text);
    const chips = parts.flatMap((part): PromptContext[] =>
      part.kind === "paste"
        ? [{ kind: "paste" as const, id: crypto.randomUUID(), text: part.text }]
        : part.kind === "resource"
          ? [carriedResource(part)]
          : [],
    );
    const typed = parts.flatMap((part) => (part.kind === "text" ? [part.text] : [])).join("");
    composer.write({
      text: chips.length ? typed.trimEnd() : typed,
      attachments: (item.attachments ?? []).map((data) => ({ data, name: attachmentName(data) })),
      context: chips,
    });
  }
  /** Leave the edit and give the composer back its earlier draft. */
  function cancel() {
    const key = composer.owner(), current = editing();
    if (!current) return;
    finish(key);
    setError("");
    composer.write(current.stash);
  }
  /** Save the composer's text in place of the message being edited. */
  async function save(draft: Omit<Draft, "context">) {
    const key = composer.owner(), current = editing();
    if (!current) return;
    // Other queued messages may run or move meanwhile. Only a change to this
    // one, made somewhere else, stops the save: it never overwrites that.
    const now = queue().items.find((item) => item.id === current.id);
    if (!now)
      return void setError(t("This message was already sent or removed. Copy your edit, then cancel to get your draft back."));
    if (now.text !== current.original.text || JSON.stringify(now.attachments ?? []) !== JSON.stringify(current.original.attachments ?? []))
      return void setError(t("This message was changed somewhere else. Copy your edit, then cancel to see the new version."));
    try {
      await mutate({
        action: "edit",
        revision: queue().revision,
        id: current.id,
        text: draft.text,
        attachments: draft.attachments.map((item) => item.data),
      });
    } catch {
      return; // The tray already shows why, and the edit stays in the composer.
    }
    if (composer.owner() !== key) return;
    finish(key);
    composer.write(current.stash);
  }
  return {
    queue,
    busy,
    error,
    supported,
    editing,
    edit,
    cancel,
    save,
    send: (item: PendingInput) => run({ action: "steer", revision: queue().revision, id: item.id }),
    remove(item: PendingInput) {
      if (editing()?.id === item.id) cancel();
      run({ action: "remove", revision: queue().revision, id: item.id });
    },
    move(index: number, delta: number) {
      const ids = queue().items.map((item) => item.id);
      [ids[index], ids[index + delta]] = [ids[index + delta], ids[index]];
      run({ action: "reorder", revision: queue().revision, ids });
    },
  };
}

export type InputQueueState = ReturnType<typeof createInputQueue>;

/** A queued message as its row reads: what was typed, then each paste and
 * each resource read into it by its label rather than its text. */
function queuedLabel(text: string) {
  const parts = sentParts(text);
  const typed = parts.flatMap((part) => (part.kind === "text" ? [part.text] : [])).join("").trim();
  return [
    typed,
    ...parts.flatMap((part) =>
      part.kind === "paste" ? [pasteLabel(part.text)] : part.kind === "resource" ? ["@" + part.name] : [],
    ),
  ]
    .filter(Boolean)
    .join(" · ");
}

/** The tray above the composer, shown only while messages wait. */
export function InputQueue(props: { queue: InputQueueState; working: boolean }) {
  const q = props.queue;
  // Each queued prompt starts the moment the previous turn closes, so a
  // queue only reads as paused once the conversation has stayed idle.
  const [idle, setIdle] = createSignal(!props.working);
  createEffect(on(() => props.working, (working) => {
    if (working) return setIdle(false);
    const timer = setTimeout(() => setIdle(true), 600);
    onCleanup(() => clearTimeout(timer));
  }));
  // A message unchanged by an update keeps its row (and the keyboard on
  // it): each update brings every message as a new object.
  const items = createMemo<PendingInput[]>((previous = []) =>
    q.queue().items.map((item) => {
      const kept = previous.find((value) => value.id === item.id);
      return kept && JSON.stringify(kept) === JSON.stringify(item) ? kept : item;
    }),
  );
  /* Removing a message from the keyboard hands it to the row that takes
     its place, or to the message box when none does, rather than dropping
     it with the button. */
  let list: HTMLOListElement | undefined;
  /** The message removed, and the one whose row takes the keyboard. */
  const [refocus, setRefocus] = createSignal<{ removed: string; next?: string }>();
  createEffect(() => {
    const target = refocus();
    if (!target || q.busy()) return;
    const ids = items().map((item) => item.id);
    // Still there: the removal is on its way, or failed and says so.
    if (ids.includes(target.removed) && !q.error()) return;
    setRefocus(undefined);
    queueMicrotask(() => {
      const active = document.activeElement;
      if (active && active !== document.body && !list?.contains(active)) return;
      const keep = ids.includes(target.removed) ? target.removed : target.next;
      const row = keep && ids.includes(keep)
        ? list?.querySelector<HTMLButtonElement>(`[data-input-id="${CSS.escape(keep)}"] .input-queue-remove`)
        : undefined;
      (row ?? document.querySelector<HTMLTextAreaElement>("[data-composer-input]"))?.focus({ preventScroll: true });
    });
  });
  const header = () =>
    q.editing()
      ? t("Editing — sending saves it in place")
      : idle()
        ? t("Queue paused — send when ready")
        : t("Queued");
  return <Show when={q.supported() && (items().length || q.editing() || q.error())}>
    <section
      class="input-queue"
      aria-label={t("Queued messages")}
      data-state={q.editing() ? "editing" : idle() ? "paused" : "queued"}
    >
      <header>
        <span class="input-queue-title">{header()}</span>
        <Show when={!q.editing() && items().length}>
          <span class="input-queue-count">{items().length}</span>
        </Show>
        <Show when={q.editing()}>
          <Button icon="close" size="sm" class="input-queue-cancel" label={t("Cancel editing")} onClick={() => q.cancel()} />
        </Show>
      </header>
      <Show when={q.error()}><p class="input-queue-error" role="alert">{q.error()}</p></Show>
      <ol ref={list}>
        <For each={items()}>{(item, index) => {
          const active = () => q.editing()?.id === item.id;
          const attached = () => item.attachments?.length ?? 0;
          return <li class="input-queue-row" data-input-id={item.id} data-editing={active() ? "" : undefined}>
            <span class="input-queue-index" aria-hidden="true">{index() + 1}</span>
            <span class="input-queue-text" title={queuedLabel(item.text)}>
              {queuedLabel(item.text) || t("{count} attachment(s)", { count: attached() })}
            </span>
            <Show when={attached() && item.text.trim()}>
              <span class="input-queue-attached" title={t("{count} attachment(s)", { count: attached() })}>
                <Icon name="attach" size="sm" />{attached()}
              </span>
            </Show>
            <div class="input-queue-actions">
              <Show when={!active()}>
                <Show when={items().length > 1}>
                  <Button icon="move-up" size="sm" class="input-queue-reorder" label={t("Move queued message up")} disabled={q.busy() || index() === 0} onClick={() => q.move(index(), -1)} />
                  <Button icon="move-down" size="sm" class="input-queue-reorder" label={t("Move queued message down")} disabled={q.busy() || index() === items().length - 1} onClick={() => q.move(index(), 1)} />
                </Show>
                <Button icon="edit" size="sm" label={t("Edit queued message")} disabled={q.busy()} onClick={() => q.edit(item)} />
                {/* Steering goes into the work under way, which Send's arrow
                    (and the reorder chevrons) would not say. */}
                <Button icon={props.working ? "steer" : "send"} size="sm" label={props.working ? t("Steer now") : t("Send now")} disabled={q.busy()} onClick={() => q.send(item)} />
              </Show>
              <Button
                icon="trash"
                size="sm"
                class="input-queue-remove"
                label={t("Remove queued message")}
                disabled={q.busy()}
                onClick={() => {
                  if (document.activeElement?.closest(".input-queue-remove"))
                    setRefocus({ removed: item.id, next: items()[index() + 1]?.id });
                  q.remove(item);
                }}
              />
            </div>
          </li>;
        }}</For>
      </ol>
    </section>
  </Show>;
}
