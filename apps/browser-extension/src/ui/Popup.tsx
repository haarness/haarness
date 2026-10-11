/* The toolbar popup: what Haarness is doing in this browser, and the controls to stop it. */
import { For, Match, Show, Switch as Choose, type JSX } from "solid-js";
import { approvalLineKeys, type ApprovalLine } from "../core/approvals";
import type { UiApproval, UiChat, UiSnapshot } from "../core/ui";
import { Tile } from "./brand";
import { openWelcome, request, snapshot } from "./client";
import { CopyField, ErrorLine, Spinner, StatusPill, Switch, useAction } from "./components";
import { around, t } from "./i18n";
import { Icon } from "./icons";

export const SETUP_COMMAND = "haarness browser setup";

export function Popup() {
  return (
    <div class="popup">
      <header class="bar">
        <Tile size={22} />
        <span class="brand">Haarness</span>
        <span class="grow" />
        <Show when={snapshot()}>{(current) => <StatusPill pill={current().pill} />}</Show>
      </header>
      <Show when={snapshot()} fallback={<div class="loading"><Spinner /></div>}>
        {(current) => <Body snapshot={current()} />}
      </Show>
    </div>
  );
}

function Body(props: { snapshot: UiSnapshot }) {
  const s = () => props.snapshot;
  return (
    <>
      <main class="body">
        <Choose>
          <Match when={s().connection === "not-set-up"}>
            <Setup failures={s().failures} />
          </Match>
          <Match when={s().connection === "mismatch"}>
            <section class="state">
              <span class="state-icon danger"><Icon name="alert" size="lg" /></span>
              <h2>{t("Update needed")}</h2>
              <p>{t("Haarness and this extension are different versions. Update Haarness, then run setup again.")}</p>
              <CopyField value={SETUP_COMMAND} label={t("Copy command")} />
            </section>
          </Match>
          <Match when={s().stopped}>
            <Stopped />
          </Match>
          <Match when={s().connection !== "connected"}>
            <Connecting exited={s().connection === "bridge-exited"} />
          </Match>
          <Match when={true}>
            <Show when={s().update}>
              <div class="banner">
                <Icon name="refresh" size="sm" />
                <span class="grow">{t("A new version of Haarness for Chrome is ready.")}</span>
                <button type="button" class="small" onClick={() => void request({ type: "reload" })}>
                  {t("Reload")}
                </button>
              </div>
            </Show>
            <Show when={s().paused}>
              <div class="banner">
                <Icon name="pause" size="sm" />
                <span class="grow">{t("Paused. Agents can't use this browser until you turn it back on.")}</span>
              </div>
            </Show>
            <For each={s().approvals}>{(approval) => <Approval approval={approval} />}</For>
            <Show when={s().chats.length} fallback={<Empty projects={s().projects.length} />}>
              <Chats chats={s().chats} />
            </Show>
          </Match>
        </Choose>
      </main>
      <Footer snapshot={s()} />
    </>
  );
}

function Setup(props: { failures: number }) {
  return (
    <section class="state setup" aria-labelledby="setup-title">
      <span class="state-icon"><Icon name="terminal" size="lg" /></span>
      <h2 id="setup-title">{t("Finish setup")}</h2>
      <p>{t("Chrome can't reach Haarness yet. Run this once in a terminal:")}</p>
      <CopyField value={SETUP_COMMAND} label={t("Copy command")} />
      <p class="muted small">{t("In Haarness Desktop: Settings → Browser → Chrome")}</p>
      <div class="actions">
        <button type="button" onClick={openWelcome}>
          {t("Open setup guide")}
        </button>
        <button type="button" class="quiet" onClick={() => void request({ type: "retry" })}>
          <Icon name="refresh" size="sm" />
          {t("Check again")}
        </button>
      </div>
      <Show when={props.failures >= 3}>
        <p class="hint">{t("Still not connected after setup? Your organization may block this; check chrome://policy.")}</p>
      </Show>
    </section>
  );
}

function Connecting(props: { exited: boolean }) {
  return (
    <section class="state" aria-live="polite">
      <span class="state-icon"><Spinner /></span>
      <h2>{t("Connecting to Haarness…")}</h2>
      <p>{props.exited ? t("Haarness closed the connection. Trying again…") : t("This takes a moment after Chrome starts.")}</p>
      <div class="actions">
        <button type="button" class="quiet" onClick={() => void request({ type: "retry" })}>
          <Icon name="refresh" size="sm" />
          {t("Check again")}
        </button>
      </div>
    </section>
  );
}

function Stopped() {
  const action = useAction();
  return (
    <section class="state stopped" aria-labelledby="stopped-title">
      <span class="state-icon danger"><Icon name="pause" size="lg" /></span>
      <h2 id="stopped-title">{t("You stopped Haarness")}</h2>
      <p>{t("Agents can't use this browser until you resume. Their tabs stay open.")}</p>
      <div class="actions">
        <button type="button" class="primary" disabled={action.busy()} onClick={() => void action.run(() => request({ type: "resume" }))}>
          <Icon name="resume" size="sm" />
          {t("Resume")}
        </button>
      </div>
    </section>
  );
}

/** "Use example.com in Chrome", with the site set apart (and kept left to right). */
export function ApprovalText(props: { line: ApprovalLine }): JSX.Element {
  const site = () => props.line.origin.replace(/^https:\/\//, "");
  return around(t(approvalLineKeys[props.line.category]), "{origin}", () => (
    <strong class="ltr origin" title={props.line.origin}>
      {site()}
    </strong>
  ));
}

function Approval(props: { approval: UiApproval }) {
  const action = useAction();
  const answer = (decision: "always" | "reject") =>
    void action.run(() => request({ type: "answer", server: props.approval.server, permission: props.approval.id, decision }));
  return (
    <section class="approval" aria-label={t("Waiting for you")}>
      <div class="eyebrow">
        <Icon name="safe" size="sm" />
        {t("Waiting for you")}
      </div>
      <p class="approval-chat">
        <span class="chat-name" dir="auto">{props.approval.title || t("Untitled chat")}</span>
        <span class="sep" aria-hidden="true">·</span>
        <span class="project" dir="auto">{props.approval.project}</span>
      </p>
      <ul class="approval-lines">
        <For each={props.approval.lines}>
          {(line) => (
            <li>
              <ApprovalText line={line} />
            </li>
          )}
        </For>
      </ul>
      <Show
        when={props.approval.answerable}
        fallback={<p class="hint">{t("This request also needs an approval outside Chrome. Answer it in Haarness.")}</p>}
      >
        <div class="actions">
          <button type="button" class="primary" disabled={action.busy()} onClick={() => answer("always")}>
            {t("Allow in this chat")}
          </button>
          <button type="button" disabled={action.busy()} onClick={() => answer("reject")}>
            {t("Deny")}
          </button>
        </div>
        <p class="hint">{t("or answer in Haarness")}</p>
      </Show>
      <Show when={action.error()}>{(message) => <ErrorLine>{message()}</ErrorLine>}</Show>
    </section>
  );
}

function Chats(props: { chats: UiChat[] }) {
  const closeAll = useAction();
  return (
    <section class="chats" aria-labelledby="chats-title">
      <h2 class="section-label" id="chats-title">
        {t("Chats using Chrome")}
      </h2>
      <ul class="chat-list">
        <For each={props.chats}>{(chat) => <Chat chat={chat} />}</For>
      </ul>
      <button
        type="button"
        class="quiet close-all"
        disabled={closeAll.busy()}
        onClick={() => void closeAll.run(() => request({ type: "close-all" }))}
      >
        <Icon name="close" size="sm" />
        {t("Close all agent tabs")}
      </button>
    </section>
  );
}

function Chat(props: { chat: UiChat }) {
  const action = useAction();
  const c = () => props.chat;
  return (
    <li class="chat" classList={{ working: c().working }}>
      <div class="chat-head">
        <span class="chat-title" dir="auto">{c().title || t("Untitled chat")}</span>
        <span class={"pill small " + (c().working ? "working" : "idle")}>
          <span class="dot" aria-hidden="true" />
          {c().working ? t("Working…") : t("Idle")}
        </span>
      </div>
      <div class="chat-meta">
        <Icon name="project" size="xs" />
        <span class="project" dir="auto">{c().project}</span>
        <span class="sep" aria-hidden="true">·</span>
        <span>{c().tabs === 1 ? t("{count} tab", { count: 1 }) : t("{count} tabs", { count: c().tabs })}</span>
        <Show when={c().subagent}>
          <span class="sep" aria-hidden="true">·</span>
          <span>{t("Subagent")}</span>
        </Show>
      </div>
      <Show when={c().refused}>
        {(refused) => (
          <p class="note">
            {refused() === "full"
              ? t("This chat already has 32 tabs, so the tab you added stays yours.")
              : t("Haarness can't use browser or extension pages, so the tab you added stays yours.")}
          </p>
        )}
      </Show>
      <div class="chat-actions">
        <button
          type="button"
          class="small"
          disabled={action.busy()}
          onClick={() => void action.run(() => request({ type: "show-tabs", key: c().key }))}
        >
          <Icon name="tabs" size="sm" />
          {t("Show tabs")}
        </button>
        <Show when={c().working}>
          <button
            type="button"
            class="small danger"
            disabled={action.busy()}
            onClick={() => void action.run(() => request({ type: "stop-chat", key: c().key }))}
          >
            <Icon name="stop" size="xs" filled />
            {t("Stop")}
          </button>
        </Show>
        <span class="grow" />
        <button
          type="button"
          class="small quiet"
          disabled={action.busy()}
          onClick={() => void action.run(() => request({ type: "close-tabs", key: c().key }))}
        >
          {t("Close tabs")}
        </button>
      </div>
      <Show when={action.error()}>{(message) => <ErrorLine>{message()}</ErrorLine>}</Show>
    </li>
  );
}

function Empty(props: { projects: number }) {
  return (
    <section class="empty">
      <span class="empty-art" aria-hidden="true">
        <Icon name="tabs" size="xl" />
      </span>
      <h2>{t("No chats are using Chrome yet")}</h2>
      <p>{t("Ask an agent to open a website in Chrome.")}</p>
      <Show when={!props.projects}>
        <p class="muted small">{t("Open a project in Haarness to get started.")}</p>
      </Show>
    </section>
  );
}

function Footer(props: { snapshot: UiSnapshot }) {
  const s = () => props.snapshot;
  return (
    <footer class="foot">
      <Show when={s().connection !== "not-set-up" && s().connection !== "mismatch" && !s().stopped}>
        <label class="switch-row">
          <span>{t("Let Haarness use this browser")}</span>
          <Switch
            label={t("Let Haarness use this browser")}
            checked={!s().paused}
            onChange={(on) => void request({ type: "pause", paused: !on })}
          />
        </label>
      </Show>
      <div class="links">
        <button type="button" class="quiet" onClick={openWelcome}>
          <Icon name="settings" size="sm" />
          {t("Settings & help")}
        </button>
        <span class="grow" />
        <Show when={s().shortcut}>
          {(shortcut) => (
            <span class="shortcut">
              {t("Stop all")}
              <kbd>{shortcut()}</kbd>
            </span>
          )}
        </Show>
      </div>
    </footer>
  );
}
