/*
 * The welcome and options page: live setup steps, what Haarness may do in
 * Chrome, this browser's settings, and help for when it does not connect.
 */
import { createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import { key, languages } from "../core/i18n";
import type { UiSnapshot } from "../core/ui";
import { Tile } from "./brand";
import { request, snapshot } from "./client";
import { CopyField, Spinner, StatusPill, Switch } from "./components";
import { language, t } from "./i18n";
import { Icon, type IconName } from "./icons";
import { SETUP_COMMAND } from "./Popup";

export function Welcome() {
  const [pinned, setPinned] = createSignal<boolean>();
  const check = () =>
    void chrome.action
      .getUserSettings()
      .then((settings) => setPinned(settings.isOnToolbar))
      .catch(() => setPinned(undefined));
  check();
  const timer = setInterval(check, 2_000);
  onCleanup(() => clearInterval(timer));

  return (
    <div class="welcome">
      <header class="top">
        <div class="top-inner">
          <Tile size={26} />
          <span class="brand">Haarness for Chrome</span>
          <span class="grow" />
          <Show when={snapshot()}>{(current) => <StatusPill pill={current().pill} />}</Show>
        </div>
      </header>
      <main class="page">
        <section class="hero">
          <h1>{t("Use Chrome with Haarness")}</h1>
          <p class="lead">
            {t("Let Haarness agents work in your own Chrome — with your sign-ins, in tab groups you can see, and only on sites you allow.")}
          </p>
        </section>
        <Show when={snapshot()} fallback={<div class="loading"><Spinner /></div>}>
          {(current) => (
            <div class="columns">
              <div class="main-column">
                <Steps snapshot={current()} pinned={pinned()} />
                <Capabilities />
                <Troubleshooting />
              </div>
              <aside class="side-column">
                <Settings snapshot={current()} />
                <Projects snapshot={current()} />
                <Versions snapshot={current()} />
              </aside>
            </div>
          )}
        </Show>
      </main>
    </div>
  );
}

function Steps(props: { snapshot: UiSnapshot; pinned: boolean | undefined }) {
  const s = () => props.snapshot;
  const installed = () => s().connection === "connected" || s().connection === "mismatch";
  const projects = () => s().projects.filter((project) => project.state === "connected").length;
  const tried = () => s().chats.length > 0;
  return (
    <section class="card steps" aria-labelledby="steps-title">
      <div class="card-head">
        <h2 id="steps-title">{t("Get set up")}</h2>
        <Show when={!installed()}>
          <button type="button" class="small quiet" onClick={() => void request({ type: "retry" })}>
            <Icon name="refresh" size="sm" />
            {t("Check again")}
          </button>
        </Show>
      </div>
      <ol class="step-list">
        <Step number={1} done={installed()} title={t("Haarness is installed")}>
          <Show
            when={installed()}
            fallback={
              <>
                <p>{t("Run this once in a terminal, then come back:")}</p>
                <CopyField value={SETUP_COMMAND} label={t("Copy command")} />
                <p class="muted">{t("In Haarness Desktop: Settings → Browser → Chrome")}</p>
                <Show when={s().failures >= 3}>
                  <p class="muted">{t("Still not connected after setup? Your organization may block this; check chrome://policy.")}</p>
                </Show>
              </>
            }
          >
            <p>{t("Chrome is connected to Haarness on this computer.")}</p>
          </Show>
        </Step>
        <Step number={2} done={projects() > 0} title={t("A project is open")}>
          <p>
            {projects() > 0
              ? projects() === 1
                ? t("{count} project is open.", { count: 1 })
                : t("{count} projects are open.", { count: projects() })
              : t("Open a project in Haarness Desktop, the TUI or the CLI.")}
          </p>
        </Step>
        <Step number={3} done={props.pinned === true} title={t("Pin Haarness to the toolbar")}>
          <p>
            {props.pinned
              ? t("Haarness is on the toolbar.")
              : t("Click the puzzle piece in Chrome's toolbar, then the pin next to Haarness. The button shows when agents are working and when they need you.")}
          </p>
        </Step>
        <Step number={4} done={tried()} title={t("Try it")}>
          <p>{tried() ? t("An agent has used Chrome.") : t("Ask an agent: “Open example.com in Chrome and summarise it.”")}</p>
        </Step>
      </ol>
    </section>
  );
}

function Step(props: { number: number; done: boolean; title: string; children: JSX.Element }) {
  return (
    <li class="step" classList={{ done: props.done }}>
      <span class="step-mark" aria-hidden="true">
        <Show when={props.done} fallback={props.number}>
          <Icon name="check" size="sm" />
        </Show>
      </span>
      <div class="step-body">
        <h3>
          {props.title}
          <span class="sr-only">{props.done ? t("Done") : t("Not done yet")}</span>
        </h3>
        {props.children}
      </div>
    </li>
  );
}

function Capabilities() {
  const items: { icon: IconName; title: string; text: string }[] = [
    {
      icon: "tabs",
      title: key("Only its own tabs"),
      text: key("Each chat works in its own tab group. Drag a tab in to share it; drag it out or close it to take it back."),
    },
    {
      icon: "safe",
      title: key("Asks first, per chat"),
      text: key("Agents ask before each new site and before running scripts on a page. You answer per chat, and the answer lasts until Haarness restarts."),
    },
    {
      icon: "pause",
      title: key("You stay in control"),
      text: key("Chrome shows a “started debugging this browser” bar while Haarness works. Cancel on that bar stops Haarness in this browser until you resume. You can also pause it, or stop a chat, from the toolbar button."),
    },
    {
      icon: "hidden",
      title: key("Uses sites as you"),
      text: key("Snapshots hide password fields. On sites you approve an agent can use the site as you, including running scripts, so approve only sites you'd let it use."),
    },
  ];
  return (
    <section class="section" aria-labelledby="can-title">
      <h2 id="can-title">{t("What Haarness can do in Chrome")}</h2>
      <ul class="features">
        <For each={items}>
          {(item) => (
            <li>
              <span class="feature-icon" aria-hidden="true">
                <Icon name={item.icon} size="md" />
              </span>
              <div>
                <h3>{t(item.title)}</h3>
                <p>{t(item.text)}</p>
              </div>
            </li>
          )}
        </For>
      </ul>
    </section>
  );
}

function Troubleshooting() {
  return (
    <section class="section" aria-labelledby="help-title">
      <h2 id="help-title">{t("Troubleshooting")}</h2>
      <dl class="help">
        <dt>{t("Keep Developer mode on")}</dt>
        <dd>{t("Chrome turns off unpacked extensions when Developer mode is off. Keep it on in chrome://extensions.")}</dd>
        <dt>{t("Managed Chrome")}</dt>
        <dd>{t("If your organization manages Chrome, it may block Haarness or Developer mode. Check chrome://policy.")}</dd>
        <dt>{t("Still stuck?")}</dt>
        <dd class="help-command">
          <span>{t("This shows what Chrome and Haarness can find:")}</span>
          <CopyField value="haarness browser status" label={t("Copy command")} />
        </dd>
      </dl>
    </section>
  );
}

function Settings(props: { snapshot: UiSnapshot }) {
  const settings = () => props.snapshot.settings;
  const save = (change: Partial<UiSnapshot["settings"]>) => void request({ type: "settings", settings: change });
  return (
    <section class="card" aria-labelledby="settings-title">
      <h2 id="settings-title">{t("Settings")}</h2>
      <div class="field">
        <label for="language">{t("Language")}</label>
        <select id="language" value={language()} onChange={(event) => save({ language: event.currentTarget.value })}>
          <For each={languages}>{(entry) => <option value={entry.code}>{entry.label}</option>}</For>
        </select>
      </div>
      <div class="field">
        <label for="label">{t("Profile label")}</label>
        <input
          id="label"
          type="text"
          maxLength={64}
          placeholder={t("Work")}
          value={settings().label}
          onChange={(event) => save({ label: event.currentTarget.value.trim() })}
        />
        <p class="field-help">{t("Shown in Haarness when more than one Chrome profile is connected.")}</p>
      </div>
      <div class="field toggle">
        <div>
          <label for="cursor">{t("Show the agent’s cursor")}</label>
          <p class="field-help">
            {t("See where Haarness points, clicks and types while it controls the browser. Turning it off makes browser actions a little faster.")}
          </p>
        </div>
        <Switch
          id="cursor"
          label={t("Show the agent’s cursor")}
          checked={settings().cursor}
          onChange={(cursor) => save({ cursor })}
        />
      </div>
    </section>
  );
}

function Projects(props: { snapshot: UiSnapshot }) {
  return (
    <section class="card" aria-labelledby="projects-title">
      <h2 id="projects-title">{t("Connected projects")}</h2>
      <Show
        when={props.snapshot.projects.length}
        fallback={<p class="muted small">{props.snapshot.connection === "connected" ? t("No projects are open.") : t("Connect Haarness first.")}</p>}
      >
        <ul class="projects">
          <For each={props.snapshot.projects}>
            {(project) => (
              <li>
                <Icon name="project" size="sm" />
                <div class="project-text">
                  <span class="project-name" dir="auto">{project.name}</span>
                  <span class="project-path ltr" title={project.workspace}>
                    {project.workspace}
                  </span>
                  <Show when={project.state === "error" && project.error}>
                    {(error) => <span class="project-error">{t(error())}</span>}
                  </Show>
                </div>
                <span class={"pill small " + (project.state === "connected" ? "connected" : project.state === "error" ? "stopped" : "connecting")}>
                  <span class="dot" aria-hidden="true" />
                  {project.state === "connected" ? t("Connected") : project.state === "error" ? t("Error") : t("Connecting…")}
                </span>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
}

function Versions(props: { snapshot: UiSnapshot }) {
  const v = () => props.snapshot.versions;
  return (
    <section class="card" aria-labelledby="versions-title">
      <h2 id="versions-title">{t("Versions")}</h2>
      <dl class="versions">
        <dt>Haarness for Chrome</dt>
        <dd class="ltr">{v().extension}</dd>
        <dt>Haarness</dt>
        <dd class="ltr">{v().haarness ?? "—"}</dd>
        <dt>{props.snapshot.browserName}</dt>
        <dd class="ltr">{v().browser || "—"}</dd>
      </dl>
      <Show when={props.snapshot.extensionPath}>
        {(path) => (
          <p class="field-help folder">
            <span>{t("Extension folder")}</span>
            <span class="ltr path">{path()}</span>
          </p>
        )}
      </Show>
    </section>
  );
}
