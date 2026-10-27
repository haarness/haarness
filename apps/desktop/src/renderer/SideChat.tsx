import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { createStore, produce, unwrap } from "solid-js/store";
import { t, errorLine } from "./i18n";
import { Icon } from "./icons";
import { Markdown } from "./Markdown";
import { Shimmer } from "./Shimmer";
import { useApp } from "./state";
import { cancelled } from "./error-text";
import { registerCyclePanel, tabButton } from "./tab-cycle";
import { shortcutLabel } from "./search";
import {
  type SideChats,
  type SideTurn,
  btwQuestion,
  sideAdd,
  sideBusy,
  sideHistory,
  sideRemove,
  sideRestore,
  sideStored,
  sideTitle,
} from "./side-chat";
import "./side-chat.css";

export { btwQuestion };

/*
 * The store. Side chats outlive the panel: an answer keeps streaming while the
 * panel is put away or another chat is shown, and comes back with the chat.
 * An answer still being written is kept apart (`live`) until it finishes, so
 * its deltas do not write the stored threads on every token.
 */
const [store, setStore] = createStore<{ chats: Record<string, SideChats>; live: Record<string, string> }>({
  chats: {},
  live: {},
});
const loads = new Map<string, Promise<void>>();
/** Answers being written: the stream id the main process knows each by. */
const streams = new Map<string, { key: string; thread: string }>();
const running = new Map<string, string>();
const saves = new Map<string, ReturnType<typeof setTimeout>>();
/** Which thread's box takes the keyboard next, and when that was asked. */
const [want, setWant] = createSignal<{ id: string; at: number }>();
/** Bumped when something asks for the side chat to come on screen. */
const [shown, setShown] = createSignal(0);
/** Bumped when a chat's last side chat closed, which puts the panel away. */
const [emptied, setEmptied] = createSignal(0);
export const sideChatRequested = shown;
export const sideChatEmptied = emptied;

/** One chat's side chats: its project connection and the chat. */
export const sideChatKey = (connection: string, thread: string) => connection + ":" + thread;

let listening = false;
function listen() {
  if (listening) return;
  listening = true;
  window.haarness.onEvent((event) => {
    if (event.kind !== "btw" || !event.id) return;
    const owner = streams.get(event.id);
    if (!owner) return;
    const data = event.data as { type: "delta" | "text"; text: string };
    if (data.type === "delta") grow(owner.thread, (store.live[owner.thread] ?? "") + data.text);
    else if (data.type === "text") grow(owner.thread, data.text);
  });
}

function grow(thread: string, text: string) {
  if (store.live[thread] !== undefined) setStore("live", thread, text);
}

/** Ends thread `thread`'s answer as `turn` says, if it is still being written. */
function settle(key: string, thread: string, turn: Partial<SideTurn>) {
  const threads = store.chats[key]?.threads;
  const at = threads?.findIndex((item) => item.id === thread) ?? -1;
  const last = at < 0 ? undefined : threads![at].turns.length - 1;
  if (last === undefined || last < 0 || threads![at].turns[last].state !== "pending") return;
  setStore("chats", key, "threads", at, "turns", last, { answer: store.live[thread] ?? "", ...turn });
  setStore("live", produce((live) => void delete live[thread]));
  running.delete(thread);
  save(key);
}

/** Stored 300 ms after the last change, and once more as the page goes away. */
function save(key: string) {
  clearTimeout(saves.get(key));
  saves.set(
    key,
    setTimeout(() => {
      saves.delete(key);
      write(key);
    }, 300),
  );
}
/** Saving counts as using the chat: the main process keeps the side chats of
 * the chats used most recently (see main/side-chat-storage.ts). */
function write(key: string) {
  const chat = store.chats[key];
  if (!chat) return;
  // The store's own objects: its proxies cannot cross to the main process.
  void window.haarness.sideChats(key, sideStored(unwrap(chat))).catch(() => {});
}
window.addEventListener("pagehide", () => {
  for (const [key, timer] of saves) {
    clearTimeout(timer);
    write(key);
  }
  saves.clear();
});

/** Reads chat `key`'s stored side chats, once. A write before then would lose them. */
function load(key: string) {
  let pending = loads.get(key);
  if (!pending) {
    pending = window.haarness
      .sideChats(key)
      .catch(() => null)
      .then((value) => {
        if (!store.chats[key]) setStore("chats", key, sideRestore(value));
      });
    loads.set(key, pending);
  }
  return pending;
}

const index = (key: string, id: string) => store.chats[key]?.threads.findIndex((item) => item.id === id) ?? -1;
const thread = (key: string, id: string) => store.chats[key]?.threads.find((item) => item.id === id);

function create(key: string) {
  const id = crypto.randomUUID();
  setStore("chats", key, (chat) => sideAdd(chat ?? { threads: [] }, { id, turns: [], draft: "", time: Date.now() }));
  save(key);
  return id;
}

/**
 * Brings chat `connection`/`chat`'s side chat on screen with its box focused:
 * the thread in front, else the newest, else a new one; `fresh` always starts
 * a new one. `question` is then asked there, or left in its box while that
 * thread is still answering.
 */
export async function openSideChat(input: { connection: string; chat: string; fresh?: boolean; question?: string }) {
  const key = sideChatKey(input.connection, input.chat);
  await load(key);
  const chat = store.chats[key];
  const id = input.fresh ? create(key) : (chat?.front ?? chat?.threads.at(-1)?.id ?? create(key));
  setStore("chats", key, "front", id);
  setShown((value) => value + 1);
  setWant({ id, at: Date.now() });
  const question = input.question?.trim();
  if (!question) return;
  if (sideBusy(thread(key, id))) return setDraft(key, id, question);
  void ask(key, id, question);
}

function setDraft(key: string, id: string, text: string) {
  const at = index(key, id);
  if (at < 0 || store.chats[key].threads[at].draft === text) return;
  setStore("chats", key, "threads", at, "draft", text);
  save(key);
}

/** Streams the answer to `question` into thread `id`. */
async function ask(key: string, id: string, question: string) {
  const current = thread(key, id);
  if (!current || sideBusy(current)) return;
  listen();
  const history = sideHistory(current.turns);
  const at = index(key, id);
  setStore("chats", key, "threads", at, "turns", current.turns.length, { question, answer: "", state: "pending" });
  setStore("live", id, "");
  save(key);
  const stream = crypto.randomUUID();
  streams.set(stream, { key, thread: id });
  running.set(id, stream);
  const [connection, chat] = [key.slice(0, key.lastIndexOf(":")), key.slice(key.lastIndexOf(":") + 1)];
  try {
    const end = await window.haarness.btw(connection, chat, history.length ? { question, history } : { question }, stream);
    if (end?.type === "done") settle(key, id, { answer: end.answer, state: "done" });
    else settle(key, id, { state: "error", error: end?.message ?? t("The answer ended before it finished.") });
  } catch (error) {
    // Cancelled without Stop (the project closed meanwhile): stopped, not failed.
    settle(key, id, cancelled(error) ? { state: "stopped" } : { state: "error", error: errorLine(error) });
  } finally {
    streams.delete(stream);
    if (running.get(id) === stream) running.delete(id);
  }
}

function stop(key: string, id: string) {
  const stream = running.get(id);
  settle(key, id, { state: "stopped" });
  if (stream) void window.haarness.cancel(stream).catch(() => {});
}

/** Asks the last question of thread `id` again, in place of an answer that failed or was stopped. */
function retry(key: string, id: string) {
  const current = thread(key, id);
  const last = current?.turns.at(-1);
  if (!current || !last || last.state === "pending" || last.state === "done") return;
  setStore("chats", key, "threads", index(key, id), "turns", (turns) => turns.slice(0, -1));
  void ask(key, id, last.question);
}

/** Closes thread `id`'s tab, which discards it. With none left, the panel goes away. */
function close(key: string, id: string) {
  if (sideBusy(thread(key, id))) stop(key, id);
  setStore("chats", key, (chat) => sideRemove(chat, id));
  save(key);
  if (!store.chats[key].threads.length) setEmptied((value) => value + 1);
}

/** What a turn says so far: its answer, or the one still being written. */
const answer = (id: string, turn: SideTurn) => (turn.state === "pending" ? (store.live[id] ?? "") : turn.answer);

/** Within this many pixels of the bottom, a growing answer keeps the view pinned to it. */
const PIN = 32;

/** Closes the thread in front of chat `connection`/`chat`'s side chat (⌘W inside it). */
export function closeFrontSideThread(connection: string, chat: string) {
  const key = sideChatKey(connection, chat);
  const value = store.chats[key];
  const id = value?.threads.find((item) => item.id === value.front)?.id ?? value?.threads.at(-1)?.id;
  if (id) close(key, id);
}

/** The side chat: one tab per thread, the thread in front, and its box.
 * `onEscape` is Esc in an idle box. */
export function SideChatPanel(props: { onHide: () => void; onEscape: () => void }) {
  const app = useApp();
  const key = createMemo(() =>
    app.state.connection && app.state.selected ? sideChatKey(app.state.connection.id, app.state.selected) : undefined,
  );
  const chat = () => (key() ? store.chats[key()!] : undefined);
  const threads = () => chat()?.threads ?? [];
  const front = () => {
    const value = chat();
    return value?.threads.find((item) => item.id === value.front)?.id ?? value?.threads.at(-1)?.id;
  };
  // A panel showing a chat with no side chat yet (another chat picked, or a
  // reload after its last thread closed) starts one, leaving the keyboard
  // where it is. Closing the last tab puts the panel away instead.
  createEffect(
    on(key, (value) => {
      if (!value) return;
      void load(value).then(() => {
        if (key() === value && !store.chats[value]?.threads.length) create(value);
      });
    }),
  );
  onCleanup(
    registerCyclePanel({
      id: "side-chat",
      tabs: () => threads().map((item) => item.id),
      active: front,
      select: (id) => key() && show(key()!, id),
    }),
  );
  let strip: HTMLDivElement | undefined;
  createEffect(
    on(front, (id) => {
      if (id) requestAnimationFrame(() => tabButton(strip, id)?.scrollIntoView({ block: "nearest", inline: "nearest" }));
    }),
  );
  function show(value: string, id: string) {
    setStore("chats", value, "front", id);
    setWant({ id, at: Date.now() });
  }
  // One header row, like the session bar beside it: a tab per thread, + for
  // another, and at the far end the control that puts the side chat away.
  const Hide = () => {
    const keys = () => shortcutLabel(app.state.preferences.shortcuts.sideChat ?? "", app.state.platform);
    return (
      <button
        class="side-chat-hide"
        aria-label={t("Hide side chat")}
        title={keys() ? `${t("Hide side chat")} (${keys()})` : t("Hide side chat")}
        onClick={() => props.onHide()}
      >
        <Icon name="close" size="sm" />
      </button>
    );
  };
  return (
    <section
      class="side-chat-panel"
      data-cycle-panel="side-chat"
      aria-label={t("Side chat")}
      // The window's ⌘W closes the thread in front while the keyboard is here.
      onFocusIn={() => void window.haarness.sideChatFocus(true).catch(() => {})}
      onFocusOut={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null))
          void window.haarness.sideChatFocus(false).catch(() => {});
      }}
      onKeyDown={(event) => {
        const mod = app.state.platform === "darwin" ? event.metaKey : event.ctrlKey;
        if (!mod || event.shiftKey || event.altKey || event.key.toLowerCase() !== "w" || !key()) return;
        event.preventDefault();
        closeFrontSideThread(app.state.connection!.id, app.state.selected!);
      }}
    >
      <Show
        when={key()}
        fallback={
          <>
            <header class="side-chat-header">
              <span class="side-chat-heading">{t("Side chat")}</span>
              <Hide />
            </header>
            <div class="side-chat-empty">
              <Icon name="side-chat" size="lg" />
              <p>{t("Send a first message, then ask side questions with /btw.")}</p>
            </div>
          </>
        }
      >
        {(value) => (
          <>
            <header class="side-chat-header">
              <div class="side-chat-tabs" role="tablist" aria-label={t("Side chats")} ref={strip}>
                <For each={threads()}>
                  {(item) => (
                    <div class="side-chat-tab" classList={{ active: item.id === front() }}>
                      <button
                        class="side-chat-tab-title"
                        role="tab"
                        data-cycle-tab={item.id}
                        aria-selected={item.id === front()}
                        title={item.turns[0]?.question}
                        onClick={() => show(value(), item.id)}
                        onAuxClick={(event) => event.button === 1 && close(value(), item.id)}
                      >
                        <span class="side-chat-tab-icon" aria-hidden="true">
                          <Show when={sideBusy(item)} fallback={<Icon name="side-chat" size="sm" />}>
                            <span class="side-chat-spinner" />
                          </Show>
                        </span>
                        <span class="side-chat-tab-label">{sideTitle(item) ?? t("Side chat")}</span>
                      </button>
                      <button
                        class="side-chat-tab-close"
                        aria-label={t("Close {name}", { name: sideTitle(item) ?? t("Side chat") })}
                        title={t("Close {name}", { name: sideTitle(item) ?? t("Side chat") })}
                        onClick={() => close(value(), item.id)}
                      >
                        <Icon name="close" size="sm" />
                      </button>
                    </div>
                  )}
                </For>
                <button
                  class="side-chat-new"
                  aria-label={t("New side chat")}
                  title={t("New side chat")}
                  onClick={() => {
                    const id = create(value());
                    show(value(), id);
                  }}
                >
                  <Icon name="plus" size="sm" />
                </button>
              </div>
              <Hide />
            </header>
            <Show when={front()} keyed>
              {(id) => <SideThreadView chat={value()} id={id} onEscape={props.onEscape} />}
            </Show>
          </>
        )}
      </Show>
    </section>
  );
}

function SideThreadView(props: { chat: string; id: string; onEscape: () => void }) {
  const app = useApp();
  // A server from before side chats answers each question without the ones before it.
  const alone = () => !app.state.connection?.info.capabilities.includes("btw.stream.v1");
  const current = () => thread(props.chat, props.id);
  const turns = () => current()?.turns ?? [];
  const busy = () => sideBusy(current());
  const [text, setText] = createSignal(current()?.draft ?? "");
  let input!: HTMLTextAreaElement;
  let scroller!: HTMLDivElement;
  let content!: HTMLDivElement;
  let pinned = true;
  // The draft is stored a moment after typing stops, not on every key.
  let saving: ReturnType<typeof setTimeout> | undefined;
  const keep = (value: string) => {
    clearTimeout(saving);
    saving = setTimeout(() => {
      saving = undefined;
      setDraft(props.chat, props.id, value);
    }, 300);
  };
  onCleanup(() => {
    if (saving === undefined) return;
    clearTimeout(saving);
    setDraft(props.chat, props.id, text());
  });
  // Fits the box to what is typed, up to a few lines; an empty box is one line.
  const resize = () => {
    input.style.height = "auto";
    if (input.value) input.style.height = `${Math.min(input.scrollHeight, 180)}px`;
  };
  const bottom = () => (scroller.scrollTop = scroller.scrollHeight);
  const focus = () => {
    input.focus({ preventScroll: true });
    input.setSelectionRange(input.value.length, input.value.length);
  };
  const send = () => {
    const question = text().trim();
    if (!question || busy()) return;
    setText("");
    clearTimeout(saving);
    saving = undefined;
    setDraft(props.chat, props.id, "");
    queueMicrotask(resize);
    pinned = true;
    void ask(props.chat, props.id, question);
    requestAnimationFrame(bottom);
  };
  onMount(() => {
    resize();
    bottom();
    // Only a request just made: coming back to this chat later leaves the keyboard alone.
    const asked = want();
    if (asked?.id === props.id && Date.now() - asked.at < 2000) focus();
    // A growing answer, a new question or a resized panel keeps the bottom in
    // view while it was in view.
    const observer = new ResizeObserver(() => pinned && bottom());
    observer.observe(content);
    let width = 0;
    const box = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.width ?? 0;
      if (next === width) return;
      width = next;
      resize();
    });
    box.observe(input);
    onCleanup(() => {
      observer.disconnect();
      box.disconnect();
    });
  });
  createEffect(on(want, (value) => value?.id === props.id && focus(), { defer: true }));
  // A draft written elsewhere (a /btw that came while this thread answered) shows here.
  createEffect(
    on(
      () => current()?.draft,
      (draft) => {
        if (draft === undefined || draft === text() || saving !== undefined) return;
        setText(draft);
        queueMicrotask(resize);
      },
      { defer: true },
    ),
  );
  return (
    <div class="side-chat-thread">
      <div
        class="side-chat-scroll"
        ref={scroller}
        onScroll={() => (pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < PIN)}
      >
        <div class="side-chat-measure" ref={content}>
          <Show
            when={turns().length}
            fallback={
              <div class="side-chat-welcome">
                <Icon name="side-chat" size="lg" />
                <h2>{t("Ask a side question")}</h2>
                <p>
                  {t(
                    "Reads this chat without adding to it or interrupting it, and can't run tools. Follow-ups keep the thread going.",
                  )}
                </p>
              </div>
            }
          >
            <Show when={alone()}>
              <p class="side-chat-note">{t("This server answers each question on its own")}</p>
            </Show>
            <For each={turns()}>
              {(turn, at) => (
                <SideTurnView
                  turn={turn}
                  answer={answer(props.id, turn)}
                  last={at() === turns().length - 1}
                  onRetry={() => {
                    pinned = true;
                    retry(props.chat, props.id);
                  }}
                />
              )}
            </For>
          </Show>
        </div>
      </div>
      <form
        class="side-chat-composer"
        classList={{ running: busy() }}
        onSubmit={(event) => {
          event.preventDefault();
          if (busy()) return stop(props.chat, props.id);
          send();
        }}
        onMouseDown={(event) => {
          if (event.target instanceof HTMLElement && event.target.closest("button")) return;
          if (event.target !== input) event.preventDefault();
          focus();
        }}
      >
        <textarea
          ref={input}
          rows={1}
          value={text()}
          data-side-chat-input=""
          aria-label={t("Ask a side question")}
          placeholder={t(turns().length ? "Ask a follow-up" : "Ask a side question")}
          onInput={(event) => {
            setText(event.currentTarget.value);
            keep(event.currentTarget.value);
            resize();
          }}
          onKeyDown={(event) => {
            if (event.isComposing || event.keyCode === 229) return;
            if (event.key === "Escape") {
              // Esc stops an answer being written, and otherwise goes back to the chat.
              event.preventDefault();
              event.stopPropagation();
              if (busy()) stop(props.chat, props.id);
              else props.onEscape();
              return;
            }
            if (event.key !== "Enter" || event.shiftKey || event.altKey) return;
            event.preventDefault();
            send();
          }}
        />
        <button
          type="submit"
          class="icon-button side-chat-send"
          classList={{ stop: busy() }}
          disabled={!busy() && !text().trim()}
          aria-label={busy() ? t("Stop") : t("Send")}
          title={busy() ? t("Stop") : t("Send")}
        >
          <Icon name={busy() ? "stop" : "send"} size="sm" filled={busy()} />
        </button>
      </form>
    </div>
  );
}

function SideTurnView(props: { turn: SideTurn; answer: string; last: boolean; onRetry: () => void }) {
  const app = useApp();
  const [copied, setCopied] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(timer));
  const copy = async () => {
    await navigator.clipboard.writeText(props.answer);
    setCopied(true);
    clearTimeout(timer);
    timer = setTimeout(() => setCopied(false), 1600);
  };
  return (
    <div class="side-chat-turn">
      <article class="message user">
        <div class="bubble">
          <Markdown text={props.turn.question} />
        </div>
      </article>
      <article class="message assistant side-chat-answer">
        <Show when={props.answer}>
          <Markdown text={props.answer} live={props.turn.state === "pending"} />
        </Show>
        <Switch>
          <Match when={props.turn.state === "pending" && !props.answer}>
            <p class="side-chat-status">
              <Shimmer>{t("Thinking…")}</Shimmer>
            </p>
          </Match>
          <Match when={props.turn.state === "error"}>
            <div class="side-chat-error" role="alert">
              <p>{props.turn.error}</p>
              <Show when={props.last}>
                <button type="button" class="side-chat-retry" onClick={props.onRetry}>
                  <Icon name="refresh" size="sm" />
                  {t("Retry")}
                </button>
              </Show>
            </div>
          </Match>
          <Match when={props.turn.state === "stopped"}>
            <p class="side-chat-status">
              {t("Stopped")}
              <Show when={props.last}>
                <button type="button" class="side-chat-link" onClick={props.onRetry}>
                  {t("Retry")}
                </button>
              </Show>
            </p>
          </Match>
        </Switch>
        <Show when={props.turn.state === "done" && props.answer}>
          <div class="message-footer">
            <div class="message-tools">
              <button
                class="icon-button"
                aria-label={copied() ? t("Copied") : t("Copy message")}
                title={copied() ? t("Copied") : t("Copy message")}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => void app.run(copy)}
              >
                <Icon name={copied() ? "check" : "copy"} size="sm" />
              </button>
            </div>
          </div>
        </Show>
      </article>
    </div>
  );
}

/** Whether the side chat panel has the keyboard. */
export const sideChatFocused = () => !!document.activeElement?.closest(".side-chat-panel");
