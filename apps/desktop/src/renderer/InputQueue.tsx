import { t } from "./i18n";
import { For, Show, createEffect, createSignal, on, onCleanup } from "solid-js";
import type { EventEnvelope, InputQueue as Queue, InputQueueAction } from "@haarness/sdk";
import { useApp } from "./state";
import { Button } from "./ui";
import "./input-queue.css";

export function InputQueue(props: { working: boolean }) {
  const app = useApp();
  const [queue, setQueue] = createSignal<Queue>({ revision: 0, items: [] });
  const [editing, setEditing] = createSignal<{ id: string; revision: number; text: string }>();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const owner = () => `${app.state.connection?.id}:${app.state.selected}`;
  const supported = () => app.state.connection?.info.capabilities.includes("input-queue.v1");
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
    setQueue({ revision: 0, items: [] }); setEditing(); setError(""); setBusy(false);
    const epoch = generation;
    void refresh().catch((error) => {
      if (epoch === generation) setError(String(error));
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
  async function mutate(action: InputQueueAction) {
    const origin = owner(), epoch = generation;
    setBusy(true); setError("");
    try {
      const value = await app.api<Queue>("changeInputQueue", app.state.selected, action);
      if (origin !== owner() || epoch !== generation) return;
      accept(value); setEditing();
    } catch (error) {
      if (origin !== owner() || epoch !== generation) return;
      setError(error instanceof Error ? error.message : String(error));
      await refresh().catch(() => {});
    } finally {
      if (origin === owner() && epoch === generation) setBusy(false);
    }
  }
  function move(index: number, delta: number) {
    const ids = queue().items.map((item) => item.id);
    [ids[index], ids[index + delta]] = [ids[index + delta], ids[index]];
    void mutate({ action: "reorder", revision: queue().revision, ids });
  }
  return <Show when={supported() && (queue().items.length || editing() || error())}>
    <section class="input-queue" aria-label={t("Queued messages")}>
      <header>{queue().items.length} queued · {props.working ? "after current work" : "paused — choose Send to continue"}</header>
      <Show when={error()}><p role="alert">{error()}</p></Show>
      <For each={queue().items}>{(item, index) => <div class="input-queue-row" data-input-id={item.id}>
        <Show when={editing()?.id === item.id} fallback={<>
          <button class="input-queue-text" disabled={busy()} title="Edit queued message" onClick={() => setEditing({ id: item.id, revision: queue().revision, text: item.text })}>{item.text || `${(item.attachments?.length ?? 0)} attachment(s)`}</button>
          <Show when={(item.attachments?.length ?? 0) && item.text}><small>{(item.attachments?.length ?? 0)} attachment(s)</small></Show>
          <Button icon="up" label={t("Move queued message up")} disabled={busy() || index() === 0} onClick={() => move(index(), -1)} />
          <Button icon="down" label={t("Move queued message down")} disabled={busy() || index() === queue().items.length - 1} onClick={() => move(index(), 1)} />
          <button disabled={busy()} onClick={() => void mutate({ action: "steer", revision: queue().revision, id: item.id })}>{props.working ? "Steer" : "Send"}</button>
          <Button icon="close" label={t("Remove queued message")} disabled={busy()} onClick={() => void mutate({ action: "remove", revision: queue().revision, id: item.id })} />
        </>}>
          <textarea aria-label={t("Edit queued message")} value={editing()?.text ?? ""} onInput={(event) => setEditing((value) => value && ({ ...value, text: event.currentTarget.value }))} />
          <button disabled={busy()} onClick={() => { const value = editing(); if (value) void mutate({ action: "edit", ...value }); }}>{t("Save")}</button>
          <button disabled={busy()} onClick={() => { setEditing(); setError(""); }}>{t("Cancel")}</button>
        </Show>
      </div>}</For>
      <Show when={editing() && !queue().items.some((item) => item.id === editing()?.id)}>
        <p role="status">{t("This message was sent or removed. Your unsaved edit is kept here to copy.")}</p>
        <div class="input-queue-row">
          <textarea aria-label={t("Unsaved queued message edit")} value={editing()?.text ?? ""} onInput={(event) => setEditing((value) => value && ({ ...value, text: event.currentTarget.value }))} />
          <button onClick={() => { setEditing(); setError(""); }}>{t("Discard edit")}</button>
        </div>
      </Show>
    </section>
  </Show>;
}
