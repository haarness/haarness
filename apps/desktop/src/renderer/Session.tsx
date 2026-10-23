import { keepsRowsAbove, measureRow } from "./measure-row";
import { toastAnchor } from "./toast-anchors";
import { t, splitAround, parseLanguage, formatNumber, language } from "./i18n";
import { isOpen, setOpen } from "./disclosure";
import { InputQueue, createInputQueue } from "./InputQueue";
import { ModelPicker } from "./ModelPicker";
import {
  canRecall,
  copyEntry,
  createHistoryStore,
  historyKey,
  navigateHistory,
  type PromptHistoryEntry,
  matchingHistory,
} from "./prompt-history";
import { ConversationFind } from "./ConversationFind";
import {
  createConversationControls,
  historyCommands,
} from "./conversation-controls";
import { retainedItems } from "./conversation-history";
import { PromptContextControls, pasteLabel } from "./PromptContext";
import {
  carriedResource,
  commandPrompt,
  contextKey,
  resourceToRead,
  countLines,
  fileContext,
  mentionAgents,
  pasteFolds,
  pastedText,
  plainMessage,
  promptWithContext,
  sentParts,
} from "./prompt-context";
import { UserMessageBody } from "./UserMessage";
import { ThoughtRow, ThoughtTicker } from "./ThoughtRow";
import { latestThought } from "./thinking";
import { sessionKey } from "../projects";
import {
  isLive,
  parseShare,
  remoteAction,
  remoteAliases,
  remoteCommand,
} from "./remote-control";
import { btwQuestion, openSideChat } from "./SideChat";
import { agentColor } from "./agent-colors";
import { conversationAgents, selectConversationAgent } from "./agent-selection";
import { providerModelOptions } from "./provider-model-options";
import {
  alreadySettled,
  autoApprovalHint,
  canAutoApprove,
  describeAlwaysRule,
  reviewNoteParts,
} from "./approval-mode";
import {
  chromeApproval,
  chromeApprovalLines,
  isExplicitPermission,
  type ChromeField,
} from "../chrome";
import { chatTitle } from "./chat-title";
import { parseModel, selectModel } from "./model-selection";
import { AgentTask } from "./AgentTask";
import { Shimmer } from "./Shimmer";
import { agentTasks, taskKey, type AgentTask as Task } from "./agent-tasks";
import { createDictation } from "./dictation";
import { ProjectPicker } from "./ProjectPicker";
import { PlanDock } from "./PlanDock";
import { currentPlan } from "./plan";
import { Select } from "./Select";
import {
  For,
  Match,
  Show,
  Switch,
  createMemo,
  createSignal,
  createEffect,
  on,
  onCleanup,
  onMount,
  untrack,
} from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import type * as T from "@haarness/sdk";
import { useApp } from "./state";
import { Button, Icon, stringify, time, type IconName } from "./ui";
import { attachable, attachmentName, relabelAttachments, type Attachment } from "./media";
import { watchOpenImages } from "./ImageViewer";
import { createFileDrop, droppedPath, pathText } from "./file-drop";
import type { FileContent } from "../shared";
import { isTextEntry } from "./text-entry";
import { selection as chatSelection } from "./sidebar-selection";
import { Markdown } from "./Markdown";
import { createReplySelection } from "./reply-selection";
import { ConversationOutline } from "./ConversationOutline";
import { promptTitle, readingMark } from "./outline-layout";
import {
  keyPlace,
  movesPrompts,
  overlayOpen,
  promptTarget,
  type PromptMove,
} from "./keyboard-nav";
import { MenuButton, type MenuAction } from "./MenuButton";
import { ScreenshotPicker } from "./ScreenshotPicker";
import { Portal } from "solid-js/web";
import { isWorktree, locationName } from "../projects";
import { createWorktree } from "./worktrees";
import { ChangePreview, ToolActivity } from "./ToolActivity";
import { Mark } from "./brand";
import { matchesShortcut, shortcutLabel, shortcutText } from "./search";
import {
  describeTool,
  errorRecovery,
  permissionCommand,
  permissionScript,
  shortPath,
  toolActivity,
  toolChanges,
  toolName,
  toolResult,
} from "./transcript";

/* The plan already has a home above the composer, so its bookkeeping calls do
   not also belong in the transcript. */
const PLAN_TOOLS = ["todo", "todowrite", "todoread"];
/** How much of a step opened at the bottom of the view is brought into it. */
const OPENED_IN_SIGHT = 80;
/** How long Auto's reviewer runs before the activity line says so. */
const REVIEW_NOTICE_MS = 800;

/** Four openings for a conversation in the selected project. */
interface Starter {
  icon: IconName;
  label: string;
  detail: string;
  text: string;
}
const projectStarters: Starter[] = [
  {
    icon: "layers",
    label: "Explore code",
    detail: t("Explore and understand code"),
    text: "Explain the architecture of this project",
  },
  {
    icon: "plus",
    label: "Build a feature",
    detail: t("Build a new feature, app, or tool"),
    text: "Help me build a new feature: ",
  },
  {
    icon: "diff",
    label: "Review changes",
    detail: t("Review code and suggest changes"),
    text: "Review the current changes",
  },
  {
    icon: "code",
    label: "Fix a problem",
    detail: t("Fix issues and failures"),
    text: "Help me investigate and fix this issue: ",
  },
];

const history = createHistoryStore((key, value) =>
    window.haarness.storage(key, value),
  );

export function Session() {
  const app = useApp();
  const { state, set } = app;
  const thread = () => state.threads.find((t) => t.id === state.selected);
  // Agents' own threads are archived when their work ends and never list in
  // the sidebar, so only a conversation the reader archived says so.
  const archived = () => !!thread()?.archived && !thread()?.isSubagent;
  /** Bring an archived chat back to the sidebar. */
  async function restore(
    connection = state.connection?.id,
    id = state.selected,
  ) {
    if (!connection || !id) return;
    await window.haarness.api(connection, "updateThread", [
      id,
      { archived: false },
    ]);
    if (state.connection?.id === connection) await app.refresh();
  }
  const remoteShared = () =>
    !!state.connection &&
    !!state.selected &&
    isLive(state.remoteShares[sessionKey(state.connection.id, state.selected)]);
  const commandNoteKey = () =>
    state.connection && state.selected
      ? sessionKey(state.connection.id, state.selected)
      : "";
  // What the approval pill's mode does, worded after the server's default
  // rules (permissions/mod.rs): Review does not ask before every edit or
  // command, and browser sites ask only when Settings → Browser says so.
  const approvalModeHint = () =>
    app.isAutoApproval()
      ? autoApprovalHint(
          !!state.selected,
          // A server that asks a reviewer in Auto says so in its capabilities.
          !!state.connection?.info.capabilities.includes("approval-review.v1"),
          t,
        )
      : state.selected
        ? t("Review: edits and commands inside the project run without asking. By default this chat and its agents ask before using files outside the project, reading secrets such as .env, switching agents or generating paid media. Click to switch to Auto.")
        : t("Review: edits and commands inside the project run without asking. By default new chats here ask before using files outside the project, reading secrets such as .env, switching agents or generating paid media. Click to switch to Auto.");
  // Learn whether the chat that just opened is shared, for the composer chip,
  // and again after the event stream reconnects, since a share started or
  // stopped meanwhile sends no event. The server answers from local state: no
  // Cloud call, nothing to wait for.
  createEffect(
    on(
      () =>
        [
          state.connection?.id,
          state.selected,
          state.ready,
          state.connectionStatus === "Connected",
        ] as const,
      ([connection, selected, ready, connected]) => {
        if (!connection || !selected || !ready || !connected) return;
        const key = sessionKey(connection, selected);
        const read = app.readRemoteShare(key);
        void window.haarness
          .api(connection, "manageResource", [
            "remote-control",
            { action: "status", threadId: selected },
          ])
          .then((view) => app.setRemoteShare(key, parseShare(view), read))
          .catch(() => {});
      },
    ),
  );
  const hasForkOrigin = () =>
    !!thread() &&
    !thread()!.isSubagent &&
    !!(thread()!.parentId || thread()!.forkOrigin);
  const forkSource = () =>
    state.threads.find((t) => t.id === thread()?.parentId);
  const forkSourceTitle = () =>
    chatTitle(forkSource()?.title || thread()?.forkOrigin?.title);
  // A missing snapshot must not briefly expose a composer for a child.
  const editable = () =>
    !state.selected || (!!thread() && !thread()!.isSubagent);
  const project = app.project;
  const group = app.currentProject;
  const draftProject = app.newDraftScope;
  const draftScope = () =>
    state.selected ? (project()?.id ?? "") : draftProject();
  const [draft, setDraft] = createSignal(""),
    [effort, setEffort] = createSignal(""),
    [follow, setFollow] = createSignal(true);
  const attachments = () => app.attachmentDraft(app.draftKey());
  /** The draft the open Screenshots dialog adds to, chosen when it opened,
   * and the token main gave the Attach menu it was chosen in. */
  const [screenshotsFor, setScreenshotsFor] = createSignal<{ key: string; token: Promise<string | null> }>();
  let screenshotMenu: Promise<string | null> | undefined;
  const setAttachments = (value: Attachment[] | ((items: Attachment[]) => Attachment[])) =>
    app.setAttachmentDraft(app.draftKey(), value);
  const fileDrop = createFileDrop(
    (files, transfer) => {
      const uris = transfer.getData("text/uri-list");
      void app.run(() => receiveFiles(files, uris));
    },
    () => !!project() && editable(),
  );
  /**
   * Dropped or pasted files: media attaches to the draft. Any other file of
   * this computer inside the project is mentioned, as `@` would (a chip that
   * carries its text); one outside it, a folder or a file that is not text is
   * referred to by its path, written at the message box's caret. A remote
   * project's server cannot read this computer's files, so there they are
   * not taken, and a quiet notice says what can be attached.
   */
  async function receiveFiles(files: File[], uris = "") {
    const key = app.draftKey();
    const connection = state.connection;
    const media: File[] = [],
      paths: string[] = [];
    let refused = 0;
    for (const file of files) {
      if (await attachable(file)) {
        media.push(file);
        continue;
      }
      const path = connection?.remote ? "" : droppedPath(file, uris, window.haarness.pathForFile(file));
      if (path) paths.push(path);
      else refused++;
    }
    const root = (connection?.workspace ?? "").replace(/\/+$/, "") + "/";
    const written: string[] = [];
    for (const path of paths) {
      const mention =
        connection && root.length > 1 && path.startsWith(root)
          ? await window.haarness
              .files<FileContent>(connection.id, "read", { path: path.slice(root.length) })
              .then((file) => (file.encoding === "utf8" ? fileContext(file.path, file.content) : undefined))
              .catch(() => undefined)
          : undefined;
      // One too large to carry is named by its path instead.
      const added =
        !!mention &&
        app.draftKey() === key &&
        state.connection?.id === connection?.id &&
        (await app.addPromptContext(mention, key).then(
          () => true,
          () => false,
        ));
      if (!added) written.push(path);
    }
    if (written.length && app.draftKey() === key && composer?.isConnected && !composer.disabled)
      insertText(
        pathText(
          written,
          state.connection?.workspace ?? "",
          composer.value.slice(0, composer.selectionStart),
          composer.value.slice(composer.selectionEnd),
        ),
      );
    if (refused)
      app.notify(t("Only images, PDFs, audio and video can be attached. Mention a project file with @."), "blocked");
    if (media.length) await app.attachFiles(key, media);
  }
  const [choices, setChoices] = createSignal<
    Record<string, { provider?: string; model?: string; agent?: string }>
  >({});
  const choice = () => choices()[draftProject()] ?? {};
  const selectedAgent = () =>
    thread()?.agent ??
    selectConversationAgent(state.agents, choice().agent ?? state.connection?.info.defaultAgent ?? undefined) ??
    "build";
  const setChoice = (value: {
    provider?: string;
    model?: string;
    agent?: string;
  }) => {
    const id = draftProject();
    setChoices((items) => ({ ...items, [id]: { ...items[id], ...value } }));
  };
  /** Apply an agent the way the composer's picker does. */
  function chooseAgent(id: string) {
    void app.run(async () => {
      if (!thread()) {
        setChoice({ agent: id, provider: undefined, model: undefined });
        return;
      }
      await app.api("updateThread", state.selected, { agent: id });
      await app.refresh();
    });
  }
  /* Step through the conversation agents, as Tab does in the TUI. The list
     wraps, so pressing again walks the whole set and returns. Focus stays in
     the message box, so the change is announced as well as shown. */
  const [cycledAgent, setCycledAgent] = createSignal("");
  let cycledTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(cycledTimer));
  function cycleAgent(step: 1 | -1) {
    const available = conversationAgents(state.agents);
    if (available.length < 2) return;
    const index = available.findIndex((agent) => agent.id === selectedAgent());
    const next = available[(index + step + available.length) % available.length];
    if (!next || next.id === selectedAgent()) return;
    chooseAgent(next.id);
    setCycledAgent(next.id);
    // Clear it again so landing on the same agent later is announced too.
    clearTimeout(cycledTimer);
    cycledTimer = setTimeout(() => setCycledAgent(""), 4000);
  }
  /* The agent keys belong to the message box, not the window: anywhere else
     Tab and Shift+Tab have to keep moving focus. Popovers in the box claim
     their keys first by preventing default. */
  function agentKey(event: KeyboardEvent) {
    if (controls.busy()) return false;
    const shortcuts = state.preferences.shortcuts;
    for (const [name, step] of [
      ["agentPrev", -1],
      ["agentNext", 1],
    ] as const) {
      if (
        shortcuts[name] &&
        matchesShortcut(event, shortcuts[name], state.platform)
      ) {
        event.preventDefault();
        if (!event.repeat) cycleAgent(step);
        return true;
      }
    }
    return false;
  }
  const selectedModel = createMemo(() =>
    selectModel({
      providers: state.providers,
      explicit: thread() ?? choice(),
      agentModel: state.agents.find((agent) => agent.id === selectedAgent())
        ?.model,
      defaultProvider: state.connection?.info.defaultProvider,
      defaultModel: state.connection?.info.defaultModel,
      recent: state.modelRecents,
      threads: state.threads,
    }),
  );
  let providerSetup = "";
  createEffect(() => {
    if (!state.ready || !state.connection || state.catalogLoading) return;
    if (selectedModel()) {
      providerSetup = "";
      return;
    }
    if (providerSetup === state.connection.id) return;
    providerSetup = state.connection.id;
    // First run: nothing can answer a prompt yet, so connecting a provider is
    // the welcome screen. An open settings editor is left alone.
    if (!state.settings && !state.connect) app.openConnect({ onboarding: true });
  });
  // A model a local runtime offers is picked without a welcome, so the first
  // chats say once where replies come from and how to add a stronger model.
  const [localHintDismissed, setLocalHintDismissed] = createSignal(true);
  void window.haarness
    .storage("local-model-hint:v1")
    .then((value) => setLocalHintDismissed(value === true))
    .catch(() => {});
  const localModel = () => {
    const key = selectedModel();
    const runtime = key && localRuntimes[key.provider];
    if (!key || !runtime) return undefined;
    const elsewhere = state.providers.some(
      (provider) =>
        provider.authenticated && provider.models.length && !localRuntimes[provider.id],
    );
    return elsewhere ? undefined : { runtime, model: key.model };
  };
  const dismissLocalHint = () => {
    setLocalHintDismissed(true);
    void window.haarness.storage("local-model-hint:v1", true).catch(() => {});
  };
  const modelValue = () => {
    const key = selectedModel();
    return key ? key.provider + "/" + key.model : "";
  };
  const modelOptions = createMemo(() => providerModelOptions(state.providers));
  const [slashIndex, setSlashIndex] = createSignal(0);
  const [submissions, setSubmissions] = createSignal<string[]>([]);
  const owner = () => draftScope() + ":" + state.selected;
  const draftKey = app.draftKey;
  const submitting = () => submissions().includes(owner());
  const [slashDismissed, setSlashDismissed] = createSignal(false);
  const slashCommands = createMemo(() =>
    draft().startsWith("/") && !/\s/.test(draft()) && !slashDismissed()
      ? [
          {
            name: "memory",
            description: t("Manage project memory"),
            template: "",
          },
          { name: "language", description: t("select_language"), template: "" },
          { name: "settings", description: t("Open settings"), template: "" },
          { name: "input-mode", description: `${t("Toggle project Steer/Queue")} · /input-mode [steer|queue|toggle|reset]`, template: "" },
          { name: "btw", description: t("Ask a read-only side question without changing this transcript"), template: "" },
          {
            name: "remote-control",
            description: `${t("Continue this chat from your phone or any browser")} · /rc`,
            template: "",
            aliases: remoteAliases,
          },
          ...historyCommands,
          ...state.commands.filter(
            (item) =>
              item.name !== "language" && item.name !== "memory" && item.name !== "settings" && item.name !== "input-mode" &&
              item.name !== "remote-control" && item.name !== "btw" &&
              !historyCommands.some((command) => command.name === item.name),
          ),
        ]
          .filter((item) => {
            const query = draft().slice(1).toLowerCase();
            return (
              item.name.toLowerCase().startsWith(query) ||
              // The short form /rc once both letters are typed, so "/r"
              // still lists only the commands that start with it.
              ("aliases" in item && query.length >= 2 && item.aliases!.some((alias) => alias.startsWith(query)))
            );
          })
          .slice(0, 12)
      : [],
  );
  const slashOpen = () => slashCommands().length > 0;
  function chooseCommand(index: number) {
    const command = slashCommands()[index];
    if (command) {
      change("/" + command.name + " ");
      composer.focus();
    }
  }
  createEffect(
    on(draft, () => {
      setSlashIndex(0);
      setSlashDismissed(false);
    }),
  );
  createEffect(() => {
    if (slashOpen())
      document
        .getElementById("slash-command-" + slashIndex())
        ?.scrollIntoView({ block: "nearest" });
  });
  let scrolling!: HTMLDivElement;
  let composer!: HTMLTextAreaElement;
  let conversation!: HTMLElement;

  /** The new chat a first message is making, which the message box's text
   * moves to with the view. */
  let carriedTo: string | undefined;
  let historyIndex = -1;
  let recalled: PromptHistoryEntry[] = [];
  let savedPrompt: PromptHistoryEntry | undefined;
  let applyingHistory = false;
  const recallKey = () => historyKey(project()?.id ?? "");
  function resetRecall() {
    if (applyingHistory) return;
    historyIndex = -1;
    savedPrompt = undefined;
    recalled = [];
  }
  createEffect(
    on(owner, () => {
      resetRecall();
      if (project()) void app.run(() => history.load(recallKey()));
    }),
  );
  function recall(direction: "up" | "down") {
    if (
      !canRecall(
        direction,
        draft(),
        composer.selectionStart,
        composer.selectionEnd,
        historyIndex,
      )
    )
      return false;
    if (historyIndex === -1) recalled = matchingHistory(history.entries(recallKey(), state.selected ?? undefined), draft());
    const next = navigateHistory(
      recalled,
      historyIndex,
      direction,
    );
    if (next === undefined) return false;
    if (historyIndex === -1)
      savedPrompt = copyEntry({
        text: draft(),
        attachments: attachments(),
        context: app.promptContext(),
      });
    const entry = copyEntry(
      next === -1 ? savedPrompt! : recalled[next],
    );
    historyIndex = next;
    applyingHistory = true;
    try {
      change(entry.text);
      setAttachments(entry.attachments);
      void app.run(() => app.setPromptContext(app.draftKey(), entry.context));
    } finally {
      applyingHistory = false;
    }
    queueMicrotask(() => {
      const cursor = direction === "up" ? 0 : entry.text.length;
      composer.setSelectionRange(cursor, cursor);
    });
    return true;
  }
  const voice = createDictation({
    owner,
    visible: () =>
      !!project() && state.panel === "chat" && !state.settings && editable(),
    supported: () => state.platform === "darwin",
    textarea: () => composer,
    read: draft,
    write: (text) => change(text, true),
  });
  let scrollFrame = 0;
  let revealFrame = 0;
  let revealTarget: string | undefined;
  let lastTop = 0;
  let touchY = 0;
  // End scrolls to the bottom known when the key went down; virtual rows
  // measured meanwhile move it. Re-pin once that scroll settles.
  let settleAtEnd = false;
  function pauseFollowing() {
    settleAtEnd = false;
    revealTarget = undefined;
    cancelAnimationFrame(revealFrame);
    if (scrolling.scrollHeight <= scrolling.clientHeight + 4) return;
    cancelAnimationFrame(scrollFrame);
    scrollFrame = 0;
    setFollow(false);
  }
  /* A step or thought the reader opens or closes stays where it was clicked:
     following pins streamed output to the bottom, not the row's own growth.
     When opening it pushes the latest output out of view, following pauses,
     as scrolling up would, and Jump to latest offers the way back; when
     closing it brings the end of the chat into view, following resumes. */
  let toggled:
    | { control: Element; row: HTMLElement; top: number; height: number; until: number }
    | undefined;
  function holdToggled(event: MouseEvent) {
    const control = (event.target as Element | null)?.closest("summary, button[aria-expanded]");
    const row = control?.closest<HTMLElement>("[data-index]");
    if (!control || !row || !scrolling.contains(row)) return;
    toggled = {
      control,
      row,
      top: control.getBoundingClientRect().top,
      height: row.offsetHeight,
      // A click that opens nothing gives the view back soon.
      until: performance.now() + 400,
    };
    cancelAnimationFrame(scrollFrame);
    scrollFrame = 0;
    requestAnimationFrame(keepToggled);
  }
  function keepToggled() {
    const held = toggled;
    if (!held) return;
    const grown = held.row.isConnected ? held.row.offsetHeight - held.height : 0;
    // The body draws after the toggle event, and a thought's text after
    // that (Markdown renders in a worker).
    const drawing =
      !grown || !!held.row.querySelector('.markdown[data-markdown-ready="false"]');
    if (held.row.isConnected && drawing && performance.now() < held.until) {
      requestAnimationFrame(keepToggled);
      return;
    }
    toggled = undefined;
    if (!held.row.isConnected) return followLatest();
    const moved = held.control.getBoundingClientRect().top - held.top;
    if (Math.abs(moved) >= 1) scrolling.scrollTop += moved;
    // What opened out of sight below the view comes up just enough for its
    // first lines to show, never taking the row itself off the top.
    if (grown > 0) {
      const view = scrolling.getBoundingClientRect();
      const summary = held.control.getBoundingClientRect();
      const opened = (held.control.closest("details") ?? held.row).getBoundingClientRect();
      const wanted = Math.min(opened.bottom - summary.bottom, OPENED_IN_SIGHT);
      const lift = Math.min(wanted - (view.bottom - summary.bottom), summary.top - view.top - 8);
      if (lift > 0) scrolling.scrollTop += lift;
    }
    lastTop = scrolling.scrollTop;
    const below = scrolling.scrollHeight - scrolling.scrollTop - scrolling.clientHeight;
    if (grown > 0 && below > 4) setFollow(false);
    else if (below <= 4) setFollow(true);
    followLatest();
  }
  function followLatest() {
    if (!follow() || scrollFrame || toggled) return;
    scrollFrame = requestAnimationFrame(() => {
      scrollFrame = 0;
      // User input can arrive after a stream delta scheduled this frame.
      if (!follow() || !scrolling) return;
      scrolling.scrollTop = scrolling.scrollHeight;
      lastTop = scrolling.scrollTop;
    });
  }
  function onScroll() {
    trackOutline();
    const top = scrolling.scrollTop;
    const remaining = scrolling.scrollHeight - top - scrolling.clientHeight;
    // Height contraction and virtual-row measurement can reduce scrollTop
    // without user input. Upward input pauses synchronously in the handlers
    // below; geometry changes alone must not change the user's follow choice.
    if (!follow() && top > lastTop && remaining <= 4) setFollow(true);
    lastTop = top;
  }
  function onHistoryFocus(event: FocusEvent) {
    const target = event.target;
    if (!(target instanceof HTMLElement) || target === scrolling) return;
    const view = scrolling.getBoundingClientRect();
    const box = target.getBoundingClientRect();
    // Chromium can scroll a control into view before focusin. Compare with
    // the previous scroll position as well as its already-visible bounds.
    if (scrolling.scrollTop >= lastTop - 2 && box.top >= view.top && box.bottom <= view.bottom)
      return;
    pauseFollowing();
    lastTop = scrolling.scrollTop;
  }
  onMount(() => {
    // Markdown, images, virtual rows and panel resizing can change height
    // without a new token. Only follow these reflows while following output.
    const resize = new ResizeObserver(followLatest);
    const observe = () => {
      resize.disconnect();
      resize.observe(scrolling);
      for (const child of scrolling.children) resize.observe(child);
      followLatest();
    };
    const children = new MutationObserver(observe);
    children.observe(scrolling, { childList: true });
    observe();
    const settle = () => {
      if (!settleAtEnd) return;
      // Done at the bottom; otherwise following scrolls and settles again.
      const remaining =
        scrolling.scrollHeight - scrolling.scrollTop - scrolling.clientHeight;
      if (remaining < 1) settleAtEnd = false;
      else followLatest();
    };
    scrolling.addEventListener("scrollend", settle);
    // Before the row's own handlers, so its place is read before it changes.
    scrolling.addEventListener("click", holdToggled, true);
    onCleanup(() => {
      children.disconnect();
      resize.disconnect();
      scrolling.removeEventListener("scrollend", settle);
      scrolling.removeEventListener("click", holdToggled, true);
      cancelAnimationFrame(scrollFrame);
      toggled = undefined;
    });
  });
  let draftTimer: ReturnType<typeof setTimeout> | undefined;
  let draftSave: { key: string; text: string } | undefined;
  function persistDraft() {
    clearTimeout(draftTimer);
    const value = draftSave;
    draftSave = undefined;
    if (value)
      void app.run(() => window.haarness.storage(value.key, value.text));
  }
  window.addEventListener("pagehide", persistDraft);
  onCleanup(() => {
    window.removeEventListener("pagehide", persistDraft);
    persistDraft();
  });
  const active = () => state.turns.find((t) => t.threadId === state.selected);
  /** Page elements picked into this draft: the box then asks what to do with them. */
  const pickedElements = () =>
    app.promptContext().filter((item) => item.kind === "element").length;
  const queue = createInputQueue({
    owner,
    working: () => !!active(),
    read: () => ({ text: draft(), attachments: attachments(), context: app.promptContext() }),
    write: (value) => {
      change(value.text);
      setAttachments(value.attachments);
      void app.run(() => app.setPromptContext(app.draftKey(), value.context));
      persistDraft();
      queueMicrotask(() => composer?.focus());
    },
  });
  // While work runs, sending follows the follow-up setting; the send button
  // says which, so the user knows before they press it.
  const [followUp, setFollowUp] = createSignal<T.InputDelivery>("steer");
  function refreshFollowUp() {
    const connection = state.connection;
    if (!connection?.info.capabilities.includes("input-settings.v1")) return setFollowUp("steer");
    void window.haarness
      .api<{ data: { delivery: T.InputDelivery } }>(connection.id, "manageResource", ["input-settings", { action: "get" }])
      .then((result) => {
        if (state.connection?.id === connection.id) setFollowUp(result.data.delivery);
      })
      .catch(() => {});
  }
  createEffect(
    on(
      () => [!!active(), state.connection?.id, state.settings] as const,
      ([working, , settings]) => {
        if (working && !settings) refreshFollowUp();
      },
    ),
  );
  const showStop = () =>
    !!active() &&
    !queue.editing() &&
    !draft().trim() &&
    !attachments().length &&
    !app.promptContext().length;
  const sendLabel = () =>
    queue.editing()
      ? t("Save queued message")
      : showStop()
        ? t("Stop response")
        : t("Send message");
  // The name stays "Send"; the tooltip says what sending does mid-work.
  const sendHint = () =>
    !queue.editing() && !showStop() && active()
      ? followUp() === "queue"
        ? t("Queue message")
        : t("Steer message")
      : sendLabel();
  const [stopping, setStopping] = createSignal(false);
  const stop = async () => {
    setStopping(true);
    try {
      // An interactive Stop still answers steering the agent has not read.
      await app.api("interruptThreadTree", state.selected, { resume: true });
      await app.refresh();
    } finally {
      setStopping(false);
    }
  };
  // File → Stop response (Cmd+. on macOS, Ctrl+. elsewhere) stops the shown
  // chat's work; with nothing running it does nothing.
  onCleanup(
    window.haarness.onEvent((event) => {
      if (event.kind !== "menu" || event.data !== "stop-response") return;
      if (active() && !stopping() && editable()) void app.run(stop);
    }),
  );
  const stream = () => (active() ? state.streams[active()!.id] : undefined);
  const [findOpen, setFindOpen] = createSignal(false);
  const [findMatch, setFindMatch] = createSignal("");
  type RestoredDraft = {
    owner: string;
    text: string;
    attachments: string;
    baseText: string;
    baseAttachments: { name: string; data: string }[];
    /** Chips the undone prompt's pastes came back as. */
    pastes?: string[];
    /** Chips its resources came back as, by context key. */
    resources?: string[];
  };
  let restoredDraft: RestoredDraft | undefined;
  const pristineUndoDraft = () =>
    restoredDraft?.owner === owner() &&
    restoredDraft.text === draft() &&
    restoredDraft.attachments === JSON.stringify(attachments());
  /** What had the keyboard when Find opened, which gets it back. */
  let findOpener: HTMLElement | undefined;
  const closeFind = () => {
    setFindOpen(false);
    setFindMatch("");
    const opener = findOpener;
    findOpener = undefined;
    const back =
      opener?.isConnected && !(opener as HTMLButtonElement).disabled && opener.getClientRects().length
        ? opener
        : editable() && composer?.isConnected && !composer.disabled
          ? composer
          : scrolling;
    back?.focus({ preventScroll: true });
  };
  const openFind = () => {
    const active = document.activeElement;
    // From the palette or a menu, which close, the message box takes it back.
    if (
      !findOpen() &&
      active instanceof HTMLElement &&
      active !== document.body &&
      !active.closest("dialog, [role='dialog'], [role='menu'], .conversation-find")
    )
      findOpener = active;
    set("panel", "chat");
    setFindOpen(true);
    queueMicrotask(() =>
      document
        .querySelector<HTMLInputElement>(".conversation-find input")
        ?.focus(),
    );
  };
  const controls = createConversationControls({
    submitting,
    openFind,
    restored: (item) => {
      if (item?.data.type !== "user_message") return;
      // A slash command comes back as the line typed, so sending it again
      // runs the command; pastes come back as their chips, resources as
      // chips holding what was read, and one that could not be read as a
      // chip to read again.
      const parts = sentParts(item.data.command_line || item.data.text);
      const pastes = parts.flatMap((part) =>
        part.kind === "paste"
          ? [{ kind: "paste" as const, id: crypto.randomUUID(), text: part.text }]
          : [],
      );
      const resources = parts.flatMap((part) =>
        part.kind === "resource" ? [part.failed ? resourceToRead(part) : carriedResource(part)] : [],
      );
      const text =
        pastes.length || resources.length
          ? parts
              .flatMap((part) => (part.kind === "text" ? [part.text] : []))
              .join("")
              .trimEnd()
          : parts.map((part) => part.text).join("");
      const key = app.draftKey();
      for (const chip of [...pastes, ...resources])
        void app.run(() => app.addPromptContext(chip, key));
      const baseText = pristineUndoDraft()
        ? restoredDraft!.baseText
        : /^\/(undo|redo|compact)\s*$/.test(draft())
          ? ""
          : draft();
      const baseAttachments = pristineUndoDraft()
        ? restoredDraft!.baseAttachments
        : attachments();
      // Preserve a draft the user has already started instead of overwriting it.
      change(
        !baseText.trim() || baseText === text ? text : baseText + "\n\n" + text,
      );
      const media = item.data.attachments ?? [];
      setAttachments([
        ...baseAttachments,
        ...media
          .filter(
            (data) => !baseAttachments.some((value) => value.data === data),
          )
          .map((data) => ({ data, name: attachmentName(data) })),
      ]);
      restoredDraft = {
        owner: owner(),
        text: draft(),
        attachments: JSON.stringify(attachments()),
        baseText,
        baseAttachments,
        pastes: pastes.map((paste) => paste.id),
        resources: resources.map(contextKey),
      };
      void window.haarness.storage("history-draft:" + owner(), restoredDraft);
      persistDraft();
      setFollow(true);
      composer?.focus();
    },
    redone: () => {
      if (pristineUndoDraft()) {
        change(restoredDraft!.baseText);
        setAttachments(restoredDraft!.baseAttachments);
        const pastes = new Set(restoredDraft!.pastes ?? []);
        const resources = new Set(restoredDraft!.resources ?? []);
        const key = app.draftKey();
        if (pastes.size || resources.size)
          void app.run(() =>
            app.setPromptContext(
              key,
              app
                .promptContext()
                .filter(
                  (item) =>
                    !(item.kind === "paste" && pastes.has(item.id)) &&
                    !(item.kind === "resource" && resources.has(contextKey(item))),
                ),
            ),
          );
        persistDraft();
      }
      restoredDraft = undefined;
      void window.haarness.storage("history-draft:" + owner(), null);
    },
  });
  createEffect(
    on(owner, (key) => {
      setFindOpen(false);
      setFindMatch("");
      findOpener = undefined;
      restoredDraft = undefined;
      void window.haarness.storage("history-draft:" + key).then((value) => {
        if (
          owner() === key &&
          value &&
          typeof value === "object" &&
          "baseText" in value &&
          "baseAttachments" in value
        )
          restoredDraft = value as RestoredDraft;
      });
    }),
  );
  onMount(() => {
    const keys = (event: KeyboardEvent) => {
      if (
        event.defaultPrevented ||
        state.panel !== "chat" ||
        state.settings ||
        state.searchOpen ||
        !state.selected
      )
        return;
      const target = event.target as Element | null;
      if (
        target?.closest(
          ".cm-editor, .xterm, dialog, .select-popover, .action-menu, .project-popover",
        )
      )
        return;
      if (
        (event.metaKey || event.ctrlKey) &&
        !event.altKey &&
        !event.shiftKey &&
        event.key.toLowerCase() === "f"
      ) {
        event.preventDefault();
        openFind();
      } else if (event.key === "Escape" && findOpen()) {
        event.preventDefault();
        closeFind();
      }
    };
    document.addEventListener("keydown", keys);
    onCleanup(() => document.removeEventListener("keydown", keys));
  });
  // A paste that no text field takes (the transcript or a button has the
  // focus, or nothing does) goes where a drop would: files attach to the
  // draft, and text lands at the message box's caret. The message box and
  // other fields handle their own.
  onMount(() => {
    const paste = (event: ClipboardEvent) => {
      const data = event.clipboardData,
        target = event.target instanceof Element ? event.target : null;
      if (
        event.defaultPrevented ||
        !data ||
        !conversation.getClientRects().length ||
        (target !== document.body && !conversation.contains(target)) ||
        target?.closest("dialog, [role='dialog']") ||
        document.querySelector("dialog[open]") ||
        isTextEntry(target) ||
        isTextEntry(document.activeElement)
      )
        return;
      const files = Array.from(data.files);
      if (files.length) {
        if (!project() || !editable()) return;
        event.preventDefault();
        const uris = data.getData("text/uri-list");
        void app.run(() => receiveFiles(files, uris));
        return;
      }
      const text = data.getData("text/plain");
      if (!text || !editable() || !composer?.isConnected || composer.disabled)
        return;
      event.preventDefault();
      if (!foldPaste(text)) insertText(text);
    };
    document.addEventListener("paste", paste);
    onCleanup(() => document.removeEventListener("paste", paste));
  });
  app.registerSearch("composer", () => {
    if (!project() || !editable() || state.openingProject) return [];
    return [
      {
        key: "focus-composer",
        group: "Chat actions",
        name: t("Focus message composer"),
        keywords: "write draft input",
        icon: "compose",
        action: () => {
          set("panel", "chat");
          composer?.focus();
        },
      },
      ...(state.platform === "darwin"
        ? [
            {
              key: "dictation",
              group: "Chat actions",
              name: voice.active() ? t("Stop dictation") : t("Start dictation"),
              keywords: "microphone voice speech transcribe",
              icon: "mic" as const,
              action: async () => {
                set("panel", "chat");
                await voice.toggle();
              },
            },
          ]
        : []),
      ...(state.selected && !thread()?.isSubagent
        ? [
            {
              key: "remote-control",
              group: "Chat actions",
              name: t("Remote control"),
              detail: `${t("Continue this chat from your phone or any browser")} · /remote-control`,
              keywords: "remote control share phone mobile cloud qr rc dc",
              icon: "remote" as const,
              action: () => app.openRemoteControl(remoteShared() ? "view" : "start"),
            },
          ]
        : []),
      ...(active()
        ? [
            {
              key: "stop-turn",
              group: "Chat actions",
              name: t("Stop response"),
              keywords: "interrupt cancel turn",
              icon: "stop" as const,
              hint: shortcutLabel("Mod+.", state.platform),
              disabled: stopping(),
              action: stop,
            },
          ]
        : []),
      ...state.commands.map((command) => ({
        key: "slash:" + command.name,
        group: "Slash commands",
        name: "/" + command.name,
        detail: command.description ? t(command.description) : t("Insert into message"),
        keywords: "slash command prompt",
        icon: "code" as const,
        action: () => {
          set("panel", "chat");
          change("/" + command.name + " " + draft());
          queueMicrotask(() => composer?.focus());
        },
      })),
    ];
  });
  const tasks = createMemo(() =>
    agentTasks(retainedItems(state.items, thread())),
  );
  const plan = createMemo(() => currentPlan(state.items, thread()));
  const todos = () => plan()?.items ?? [];
  const rows = createMemo(() =>
    retainedItems(state.items, thread()).filter(
      (item) =>
        ![
          "usage",
          "snapshot",
          "queue_consumed",
          "permission_request",
          "question",
          "todo",
        ].includes(item.data.type) &&
        !(item.data.type === "user_message" && item.data.synthetic) &&
        !(item.data.type === "reasoning" && state.preferences.thinking === "hide") &&
        item.data.type !== "tool_result" &&
        !(
          item.data.type === "tool_call" &&
          PLAN_TOOLS.includes(
            toolName((item.data as { name: string }).name ?? ""),
          )
        ) &&
        // A question still being asked is the card above the message box;
        // its row comes once it is answered, with the answer.
        !(
          item.data.type === "tool_call" &&
          toolName(item.data.name) === "question" &&
          // A question announced live may not name its turn.
          state.questions.some(
            (question) =>
              question.threadId === item.threadId && (!question.turnId || question.turnId === item.turnId),
          ) &&
          !toolResult(state.items, item)
        ) &&
        (!("call_id" in item.data) ||
          !tasks().has(taskKey(item.turnId, item.data.call_id)) ||
          tasks().get(taskKey(item.turnId, item.data.call_id))?.item.id ===
            item.id),
    ),
  );
  const virtual = createVirtualizer({
    get count() {
      return rows().length;
    },
    getScrollElement: () => scrolling,
    estimateSize: () => 130,
    overscan: 8,
    getItemKey: (index) => rows()[index]?.id ?? index,
  });
  // A row wholly above the view that grows keeps what is read in place, also
  // while scrolling up (measure-row.ts). The view moves down before the list's
  // height has grown, which near its end would cut the move short, so the
  // list is made that tall first.
  virtual.shouldAdjustScrollPositionOnItemSizeChange = (item, delta, instance) => {
    if (!keepsRowsAbove(item, instance)) return false;
    const body = instance.scrollElement?.querySelector<HTMLElement>(":scope > .transcript-body");
    if (body && delta > 0) body.style.height = `${instance.getTotalSize() + delta}px`;
    return true;
  };
  // The outline lists every prompt the user wrote and marks the one being
  // read. It is measured from the virtualizer's own row offsets, at most once
  // a frame, so streaming and scrolling never read every row's layout.
  /** A prompt's first written line, or for one that is only a paste, the
   * paste's label. */
  const outlineTitle = (text: string, attachments: readonly string[]) => {
    const parts = sentParts(text);
    const paste = parts.find((part) => part.kind === "paste");
    const typed = parts.flatMap((part) => (part.kind === "text" ? [part.text] : [])).join("");
    return promptTitle(typed, attachments, attachmentName, paste ? pasteLabel(paste.text) : t("Attachment"));
  };
  const outline = createMemo(() =>
    rows().flatMap((item, index) =>
      item.data.type === "user_message"
        ? [{
            id: item.id,
            index,
            title: outlineTitle(item.data.command_line || item.data.text, item.data.attachments ?? []),
          }]
        : [],
    ),
  );
  const [readingPrompt, setReadingPrompt] = createSignal<string>();
  const [laneHeight, setLaneHeight] = createSignal(0);
  // A prompt is being read once its row reaches this far below the top.
  const readLine = 80;
  let outlineFrame = 0;
  function trackOutline() {
    if (outlineFrame || !scrolling) return;
    outlineFrame = requestAnimationFrame(() => {
      outlineFrame = 0;
      if (!scrolling) return;
      setLaneHeight(scrolling.clientHeight);
      const body = scrolling.querySelector<HTMLElement>(".transcript-body");
      const base = body
        ? body.getBoundingClientRect().top - scrolling.getBoundingClientRect().top + scrolling.scrollTop
        : 0;
      // Brings the offset cache up to date with every row measured so far.
      virtual.getTotalSize();
      const cache = virtual.measurementsCache;
      const marks = outline().flatMap((prompt) => {
        const row = cache[prompt.index];
        return row ? [{ id: prompt.id, offset: base + row.start }] : [];
      });
      setReadingPrompt(
        readingMark(marks, scrolling.scrollTop, scrolling.scrollHeight - scrolling.clientHeight, readLine),
      );
    });
  }
  createEffect(on(outline, trackOutline));
  onMount(() => {
    // The viewport and the content: rows measured after a scroll move the
    // prompts' offsets without another scroll event.
    const resize = new ResizeObserver(trackOutline);
    resize.observe(scrolling);
    const body = scrolling.querySelector(".transcript-body");
    if (body) resize.observe(body);
    onCleanup(() => {
      resize.disconnect();
      cancelAnimationFrame(outlineFrame);
    });
  });
  function jumpToPrompt(position: number) {
    const prompt = outline()[position];
    if (!prompt) return;
    cancelAnimationFrame(scrollFrame);
    scrollFrame = 0;
    setFollow(false);
    setReadingPrompt(prompt.id);
    virtual.scrollToIndex(prompt.index, { align: "start" });
  }
  /** The prompt keys: a step from the prompt being read, either end, or
   * back to following the newest output. False when there is nowhere to go. */
  function movePrompt(move: PromptMove) {
    const prompts = outline();
    const reading = prompts.findIndex((prompt) => prompt.id === readingPrompt());
    const target = promptTarget(move, prompts.length, reading, follow());
    if (target === undefined) return false;
    if (target === "latest") {
      toggled = undefined;
      setFollow(true);
      followLatest();
    } else jumpToPrompt(target);
    return true;
  }
  onMount(() => {
    const keys = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || state.panel !== "chat")
        return;
      const { promptPrev, promptNext, promptFirst, promptLast } =
        state.preferences.shortcuts;
      const move = (
        [
          ["previous", promptPrev],
          ["next", promptNext],
          ["first", promptFirst],
          ["last", promptLast],
        ] as const
      ).find(
        ([, keys]) => keys && matchesShortcut(event, keys, state.platform),
      )?.[0];
      if (
        !move ||
        !movesPrompts(
          keyPlace(document.activeElement, draft(), overlayOpen(document)),
        )
      )
        return;
      if (movePrompt(move)) event.preventDefault();
    };
    window.addEventListener("keydown", keys);
    onCleanup(() => window.removeEventListener("keydown", keys));
  });
  app.registerSearch("prompts", () =>
    outline().length
      ? (
          [
            ["prompt-previous", "Previous prompt", "promptPrev", "move-up", "previous"],
            ["prompt-next", "Next prompt", "promptNext", "move-down", "next"],
            ["prompt-first", "First prompt", "promptFirst", "up", "first"],
            ["prompt-latest", "Jump to latest", "promptLast", "jump", "last"],
          ] as const
        ).map(([key, name, shortcut, icon, move]) => ({
          key,
          group: "Chat actions",
          name: t(name),
          keywords: "message navigate scroll conversation",
          icon,
          hint: shortcutLabel(
            state.preferences.shortcuts[shortcut] ?? "",
            state.platform,
          ),
          action: () => {
            set("panel", "chat");
            movePrompt(move);
          },
        }))
      : [],
  );
  // The outline's lane sits over the transcript, so wheel input it does not
  // use scrolls the conversation here, as the user's own gesture.
  function outlineWheel(event: WheelEvent) {
    const delta =
      event.deltaMode === WheelEvent.DOM_DELTA_LINE
        ? event.deltaY * 16
        : event.deltaMode === WheelEvent.DOM_DELTA_PAGE
          ? event.deltaY * scrolling.clientHeight
          : event.deltaY;
    if (!delta) return;
    event.preventDefault();
    if (delta < 0) pauseFollowing();
    scrolling.scrollBy({ top: delta });
  }
  const revealFind = (id: string) => {
    cancelAnimationFrame(scrollFrame);
    scrollFrame = 0;
    setFollow(false);
    revealTarget = id;
    const index = rows().findIndex((item) => item.id === id);
    if (index >= 0) virtual.scrollToIndex(index, { align: "center" });
    settleReveal();
  };
  function settleReveal() {
    if (!revealTarget) return;
    cancelAnimationFrame(revealFrame);
    revealFrame = requestAnimationFrame(() => {
      if (!revealTarget || findMatch() !== revealTarget) return;
      // A cold virtual row initially has no worker-rendered content. Complete
      // the explicit search jump after mounted rows have their actual heights.
      // User scroll input cancels this intent, so late parsing cannot pull the
      // reader back after they deliberately move away.
      if (scrolling.querySelector('[data-item-id] .markdown[data-markdown-ready="false"]')) return;
      const element = scrolling.querySelector<HTMLElement>(`[data-item-id="${CSS.escape(revealTarget)}"]`);
      if (!element) return;
      const index = rows().findIndex((item) => item.id === revealTarget);
      virtual.measureElement(element);
      if (index >= 0) virtual.scrollToIndex(index, { align: "center" });
      revealTarget = undefined;
    });
  }
  // Text the person selected in a streaming reply stays selected when the
  // saved reply takes its place (see reply-selection.ts).
  const replySelection = createReplySelection();
  onCleanup(() => replySelection.dispose());
  const replyDrawn = (event: Event) =>
    replySelection.drawn(event.target as Element);
  onMount(() => {
    scrolling.addEventListener("markdown-ready", settleReveal);
    scrolling.addEventListener("markdown-ready", replyDrawn);
    onCleanup(() => {
      scrolling.removeEventListener("markdown-ready", settleReveal);
      scrolling.removeEventListener("markdown-ready", replyDrawn);
      cancelAnimationFrame(revealFrame);
    });
  });
  /* One footer per assistant turn, under its last text: when the reply
     ended, Copy for all of it and Fork. Earlier text of the turn keeps the
     footer's height with nothing in it, so the footer moving on to newer
     text never moves the transcript. */
  const turnTexts = createMemo(() => {
    const texts = new Map<string, T.Item[]>();
    for (const item of rows())
      if (item.data.type === "assistant_message") {
        const key = item.turnId ?? item.id;
        const list = texts.get(key);
        if (list) list.push(item);
        else texts.set(key, [item]);
      }
    return texts;
  });
  const reply = (item?: T.Item) => {
    if (item?.data.type !== "assistant_message") return undefined;
    const texts = turnTexts().get(item.turnId ?? item.id) ?? [item];
    return {
      last: texts.at(-1)?.id === item.id,
      text: texts.map((text) => (text.data as { text: string }).text).join("\n\n"),
    };
  };
  // Older forks retain their source link but have no recorded boundary. Put
  // their marker at the beginning rather than guessing which history was copied.
  const forkDividerIndex = createMemo(() => {
    if (!hasForkOrigin()) return -1;
    const origin = thread()?.forkOrigin;
    return origin
      ? rows().findIndex((item) => item.sequence > origin.sequence)
      : rows().length
        ? 0
        : -1;
  });
  /* The reply a fork continued from is tinted for a moment when its chat
     opens from the fork, so the eye finds it; without motion when the
     reader prefers less. */
  const [sourceReply, setSourceReply] = createSignal<string>();
  let sourceTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(sourceTimer));
  async function openForkSource() {
    const source = forkSource();
    if (!source) return;
    const origin = thread()?.forkOrigin;
    const connection = state.connection?.id;
    await app.choose(source.id);
    if (state.connection?.id !== connection || state.selected !== source.id)
      return;
    if (origin) {
      cancelAnimationFrame(scrollFrame);
      scrollFrame = 0;
      setFollow(false);
      let index = rows().findIndex((item) => item.id === origin.itemId);
      if (index < 0)
        index = rows().findLastIndex(
          (item) => item.sequence <= origin.sequence,
        );
      virtual.scrollToIndex(Math.max(0, index), { align: "center" });
      scrolling.focus({ preventScroll: true });
      clearTimeout(sourceTimer);
      setSourceReply(rows()[Math.max(0, index)]?.id);
      sourceTimer = setTimeout(() => setSourceReply(undefined), 1600);
    }
  }
  const ForkDivider = () => (
    <div class="fork-origin" role="note" aria-label={t("Chat origin")}>
      <button
        class="fork-origin-link"
        disabled={!forkSource()}
        title={
          forkSource()
            ? `Open ${forkSourceTitle()}`
            : `Source chat unavailable · ${forkSourceTitle()}`
        }
        onClick={() => app.run(openForkSource)}
      >
        <Icon name="branch" size="sm" />
        <span>{t("Continued from {title}", { title: forkSourceTitle() })}</span>
      </button>
    </div>
  );
  const visiblePermissions = () =>
    state.permissions.filter(
      (item) =>
        item.threadId === state.selected ||
        item.rootThreadId === state.selected,
    );
  const visibleQuestions = () =>
    state.questions.filter(
      (item) =>
        item.threadId === state.selected ||
        item.rootThreadId === state.selected,
    );
  const pending = () =>
    visiblePermissions().length + visibleQuestions().length > 0;
  // One decision at a time, oldest first, the way the TUI asks. The record
  // stays the same object while its id is unchanged, so a refresh does not
  // rebuild the prompt under the reader.
  const currentPermission = createMemo(
    () => visiblePermissions()[0],
    undefined,
    { equals: (a, b) => a?.permissionId === b?.permissionId },
  );
  // A request announced again (Auto's reviewer took it up, or left it to the
  // person) keeps its card; only the note follows.
  const currentReview = () => visiblePermissions()[0]?.review;
  const currentQuestion = createMemo(
    () => (visiblePermissions().length ? undefined : visibleQuestions()[0]),
    undefined,
    { equals: (a, b) => a?.questionId === b?.questionId },
  );
  createEffect(
    on(
      () => [draftScope(), state.selected] as const,
      ([, id]) => {
        // Navigation can happen before the typing debounce has saved the draft.
        persistDraft();
        const carried = carriedTo === owner() ? draft() : undefined;
        carriedTo = undefined;
        if (carried !== undefined) change(carried);
        else setDraft(state.drafts[draftKey()] ?? "");
        fileDrop.reset();
        const key = draftKey();
        const current = owner();
        const locationId = project()?.id;
        if (locationId)
          void window.haarness
            .storage("draft:" + draftScope() + ":" + id)
            .then(
              async (value) =>
                value ??
                (id
                  ? null
                  : await window.haarness.storage("draft:" + locationId + ":")),
            )
            .then((value) => {
              if (
                owner() === current &&
                !draft() &&
                typeof value === "string"
              ) {
                set("drafts", key, value);
                setDraft(value);
              }
            });
        lastTop = 0;
        setFollow(true);
      },
    ),
  );
  createEffect(
    on(
      () => state.drafts[draftKey()],
      (value) => {
        if (typeof value === "string" && value !== draft()) setDraft(value);
      },
    ),
  );
  createEffect(() => {
    rows().length;
    stream()?.text;
    stream()?.reasoning;
    virtual.getTotalSize();
    followLatest();
  });
  // A message an MCP App page asks to send goes into the message box, after
  // what is already there; the person sends it, or not.
  onMount(() => {
    const appMessage = (event: Event) => {
      const detail = (event as CustomEvent<{ text: string; accepted: boolean }>).detail;
      if (!project() || !editable() || !composer?.isConnected || composer.disabled) return;
      const current = draft();
      change(current.trim() ? current.trimEnd() + "\n\n" + detail.text : detail.text);
      composer.focus({ preventScroll: true });
      composer.setSelectionRange(composer.value.length, composer.value.length);
      detail.accepted = true;
    };
    scrolling.addEventListener("mcp-app-message", appMessage);
    onCleanup(() => scrolling.removeEventListener("mcp-app-message", appMessage));
  });
  /** Text at the message box's caret, as typing it would: Cmd+Z takes it
   * out again. */
  function insertText(text: string) {
    composer.focus({ preventScroll: true });
    if (!document.execCommand("insertText", false, text)) {
      composer.setRangeText(text, composer.selectionStart, composer.selectionEnd, "end");
      change(composer.value);
    }
  }
  /**
   * A long paste becomes one chip beside the message box, at the size the
   * TUI folds one, and is sent whole. Says whether it folded.
   */
  function foldPaste(raw: string) {
    const text = pastedText(raw);
    if (!text || !pasteFolds(text) || !project() || !editable())
      return false;
    const key = app.draftKey();
    void app.run(() =>
      app.addPromptContext({ kind: "paste", id: crypto.randomUUID(), text }, key),
    );
    composer?.focus({ preventScroll: true });
    return true;
  }
  function change(text: string, spoken = false) {
    resetRecall();
    if (!spoken) voice.cancel();
    setDraft(text);
    if (project()) {
      set("drafts", draftKey(), text);
      clearTimeout(draftTimer);
      draftSave = { key: storedDraftKey(), text };
      draftTimer = setTimeout(persistDraft, 500);
    }
  }
  /** Where the message box's text is kept between launches. */
  function storedDraftKey() {
    return "draft:" + draftScope() + ":" + state.selected;
  }
  // An input method composing text owns the message box until it ends: the
  // box's text is only replaced after that, so the composition is not lost.
  let composingText = false;
  let afterComposition: (() => void)[] = [];
  function whenComposed(run: () => void) {
    if (composingText) afterComposition.push(run);
    else run();
  }
  function compositionEnded() {
    composingText = false;
    const runs = afterComposition;
    afterComposition = [];
    // The composed text reaches the box with the input that follows.
    setTimeout(() => runs.forEach((run) => run()));
  }
  /** Takes the text being sent out of the message box, keeping whatever was
   * typed after it. */
  function takeSent(text: string, from: string) {
    whenComposed(() => {
      if (owner() !== from) return;
      const value = draft();
      if (value === text) change("");
      else if (text && value.startsWith(text)) change(value.slice(text.length).replace(/^\s+/, ""));
    });
  }
  /** Puts text that was not sent back where it was typed, ahead of anything
   * typed since; into the box when that chat is still shown. */
  function putBack(text: string, places: string[], key: string, stored: string) {
    if (!text) return;
    const joined = (value: string) => (value.trim() ? text + "\n\n" + value : text);
    if (places.includes(owner())) {
      whenComposed(() => change(joined(draft())));
      return;
    }
    const value = joined(state.drafts[key] ?? "");
    set("drafts", key, value);
    if (draftSave?.key === stored) draftSave = undefined;
    void app.run(() => window.haarness.storage(stored, value));
  }
  /** The chats whose Enter waits for the model choices. */
  const waitingForModels = new Set<string>();
  /** The send under way in each chat, which a second Enter there waits for. */
  const inFlight = new Map<string, Promise<boolean>>();
  const waitingForSend = new Set<string>();
  async function send(delivery?: T.InputDelivery): Promise<void> {
    if (queue.editing()) {
      // A queued message keeps only its pastes and the resources it held
      // among the chips: its edit saves text.
      await queue.save({
        text: promptWithContext(
          draft(),
          app
            .promptContext()
            .filter((item) => item.kind === "paste" || (item.kind === "resource" && item.block !== undefined)),
        ),
        attachments: attachments(),
      });
      return;
    }
    const remote = remoteCommand.exec(draft().trim());
    if (remote) {
      const action = remoteAction(remote[2] ?? "");
      if (!action)
        throw new Error(t("Use /remote-control, /remote-control status or /remote-control stop."));
      if (!state.connection || !state.selected)
        throw new Error(t("Send a first message, then share the chat with /remote-control."));
      if (thread()?.isSubagent)
        throw new Error(t("Share the chat that started this agent; its agents come with it."));
      change("");
      app.openRemoteControl(action);
      return;
    }
    // /btw asks in the side chat, its own panel beside the chat; a bare
    // /btw just opens it. The chat itself never sees either.
    const side = btwQuestion(draft());
    if (side !== undefined) {
      if (!state.connection) return;
      if (!state.selected)
        throw new Error(t("Send a first message, then ask side questions with /btw."));
      change("");
      await openSideChat({ connection: state.connection.id, chat: state.selected, question: side });
      return;
    }
    const localCommand = /^\/(settings|input-mode|language)(?:\s+([\s\S]*))?$/.exec(draft().trim());
    if (localCommand) {
      const connection = state.connection?.id;
      if (!connection) return;
      const original = draft();
      const commandOwner = owner();
      const argumentsText = localCommand[2]?.trim() ?? "";
      if (localCommand[1] === "language") {
        if (argumentsText) {
          const language = parseLanguage(argumentsText);
          if (!language) throw new Error(t("Choose a language with /language."));
          await app.settings({ language });
        } else {
          await app.openSettings("general", "Language");
        }
      } else if (localCommand[1] === "settings") {
        if (argumentsText) throw new Error("Use /settings to open settings, or /input-mode to toggle Steer/Queue.");
        await app.openSettings("general");
      } else {
        const result = await window.haarness.api<{ data: { message: string } }>(connection, "manageResource", ["input-settings", { action: "command", arguments: argumentsText }]);
        if (connection === state.connection?.id) {
          app.notify(result.data.message);
          refreshFollowUp();
        }
      }
      if (owner() === commandOwner && draft() === original) change("");
      return;
    }
    if (draft().trim().startsWith("/")) delivery = "steer";
    if (/^\/memory\s*$/i.test(draft())) {
      if (!state.connection) return;
      change("");
      set("memoryOpen", true);
      return;
    }
    const command = /^\/(undo|redo|compact)\s*$/.exec(draft().trim());
    if (command) {
      const commandOwner = owner();
      await controls.invoke(command[1]);
      if (owner() === commandOwner && draft().trim() === command[0].trim())
        change("");
      return;
    }
    // Enter again while a message is on its way sends this one once that
    // one has gone, however often it is pressed: the box stays editable, so
    // what is in it then is the next message. If that one did not go, its
    // text is back in the box, and nothing more is sent.
    if (submitting() && (draft().trim() || attachments().length || app.promptContext().length)) {
      const pressed = owner();
      if (waitingForSend.has(pressed)) return;
      waitingForSend.add(pressed);
      let went = false;
      try {
        went = (await inFlight.get(pressed)) ?? false;
      } finally {
        waitingForSend.delete(pressed);
      }
      if (!went || owner() !== pressed || submitting()) return;
      return send(delivery);
    }
    if (
      !project() ||
      !state.ready ||
      !editable() ||
      submitting() ||
      controls.busy() ||
      stopping() ||
      // Nothing to send: Enter in an empty box does nothing, also while work
      // runs (Stop has a button of its own, and Esc).
      (!draft().trim() &&
        !attachments().length &&
        !app.promptContext().length)
    )
      return;
    // A chat started while the model choices load would take a model chosen
    // from an incomplete (or the last project's) catalog: send once they are
    // in, checking again then, and only once however often Enter is pressed.
    // Another chat or project shown meanwhile gives it up: its draft is not
    // what Enter was pressed on, and this one stays unsent where it was.
    if (state.catalogLoading) {
      const pressed = owner();
      const connection = state.connection?.id;
      if (waitingForModels.has(pressed)) return;
      waitingForModels.add(pressed);
      try {
        await app.catalogsSettled();
      } finally {
        waitingForModels.delete(pressed);
      }
      if (owner() !== pressed || state.connection?.id !== connection) return;
      return send(delivery);
    }
    if (!selectedModel()) {
      app.openConnect({ onboarding: true });
      return;
    }
    // The local-model hint has been read once a first message goes to it.
    if (localModel() && !localHintDismissed()) dismissLocalHint();
    voice.cancel();
    let connection = project()?.id;
    let createdOwner = "";
    const options = {
      ...choice(),
      ...selectedModel(),
      agent: selectedAgent(),
    };
    const selected = state.selected;
    const wasArchived = archived();
    const origin = owner();
    const originProject = draftProject();
    const text = draft();
    const contextKey = app.draftKey();
    const storedKey = storedDraftKey();
    const keyboard = document.activeElement === composer;
    setSubmissions((items) => [...items, origin]);
    let landed!: (went: boolean) => void;
    const flight = new Promise<boolean>((done) => (landed = done));
    inFlight.set(origin, flight);
    // The message box empties as the message goes and keeps the keyboard,
    // so the next one can be typed at once. A second Enter waits for this
    // send; text that is not sent comes back.
    takeSent(text, origin);
    persistDraft();
    let sent = false;
    try {
      await app.loadAttachmentDraft(contextKey);
      await app.loadPromptContext(contextKey);
      if (app.draftKey() !== contextKey) return;
      const context = app.promptContext();
      for (const item of context)
        if (
          item.kind === "agent" &&
          !mentionAgents(state.agents).some((agent) => agent.id === item.name)
        )
          throw Error(
            `Agent ${item.name} is no longer available for delegation. Remove it from context before sending.`,
          );
      const originalAttachments = attachments();
      const media = originalAttachments.map((item) => item.data);
      const clearSentAttachments = (key: string) => {
        return app.setAttachmentDraft(key, (items) =>
          items.filter((item) => !originalAttachments.includes(item)),
        );
      };
      const historyEntry = { text, attachments: originalAttachments, context, thread: selected ?? undefined };
      void app.run(() => history.record(historyKey(connection!), historyEntry));
      persistDraft();
      setFollow(true);
      if (!state.connection && !state.openingProject) {
        if (!(await app.connect({ recent: project()!.id }, true))) return;
        connection = state.connection!.id;
      }
      // Continuing an archived chat brings it back to the sidebar, as
      // resuming one does in the TUI; the server leaves that to clients.
      if (selected && wasArchived) await restore(connection, selected);
      const threadId = await app.send(
        text,
        media,
        effort(),
        options,
        (id) => {
          void app.run(() =>
            app.setPromptContext(sessionKey(connection!, id), context),
          );
          createdOwner = connection + ":" + id;
          // The view moves to the new chat, and the box goes with it as it
          // is then, with what was typed while the chat was being made.
          if (owner() === origin) carriedTo = createdOwner;
          // Commit the destination before removing the persisted starter
          // attachments. A failed disk write must leave the original recoverable.
          void app.run(async () => {
            await app.setAttachmentDraft(sessionKey(connection!, id), originalAttachments);
            await clearSentAttachments(contextKey);
          });
          if (state.drafts["new:" + originProject] === text)
            set("drafts", "new:" + originProject, "");
          void app.run(async () => {
            await app.clearPromptContext(contextKey, context);
            await window.haarness.storage(
              "draft:" + originProject + ":",
              state.drafts["new:" + originProject] ?? "",
            );
          });
          setSubmissions((items) => [...items, createdOwner]);
          inFlight.set(createdOwner, flight);
        },
        context,
        delivery,
      );
      sent = true;
      void app.run(() =>
        history.record(historyKey(connection!), {
          text,
          attachments: originalAttachments,
          context,
          thread: threadId,
        }),
      );
      await app.clearPromptContext(contextKey, context);
      if (createdOwner)
        await app.clearPromptContext(
          sessionKey(connection!, threadId),
          context,
        );
      const shown =
        state.connection?.id === connection &&
        (state.selected === selected ||
          (!selected && state.selected === threadId));
      // A first message moves the view to its new chat; the keyboard stays
      // in the message box, unless the person has put it elsewhere since.
      if (
        shown &&
        keyboard &&
        composer?.isConnected &&
        (!document.activeElement || document.activeElement === document.body)
      )
        composer.focus({ preventScroll: true });
      // What was typed while that chat was being made went to it with the
      // view (state.tsx), so the new-chat draft starts empty again.
      if (!selected && shown) {
        set("drafts", "new:" + originProject, "");
        if (draftSave?.key === storedKey) draftSave = undefined;
        await window.haarness.storage(storedKey, "");
      }
      // app.send already clears the shared text draft. Attachments have
      // their own lifecycle, and files added during the request must survive.
      for (const key of [contextKey, createdOwner ? sessionKey(connection!, threadId) : ""]) {
        if (key) await clearSentAttachments(key);
      }
      await window.haarness.storage(
        "draft:" + connection + ":" + threadId,
        state.drafts[sessionKey(connection!, threadId)] ?? "",
      );
      if (!selected && !state.drafts["new:" + originProject])
        await window.haarness.storage("draft:" + originProject + ":", "");
    } finally {
      setSubmissions((items) =>
        items.filter((item) => item !== origin && item !== createdOwner),
      );
      // The view has moved to a new chat by now, or never will.
      carriedTo = undefined;
      for (const key of [origin, createdOwner]) if (inFlight.get(key) === flight) inFlight.delete(key);
      landed(sent);
      if (!sent) putBack(text, [origin, createdOwner], contextKey, storedKey);
    }
  }
  async function model(value: string) {
    const key = parseModel(value);
    const connection = state.connection?.id;
    if (!key || !connection) return;
    const selected = state.selected;
    const project = draftProject();
    if (selected) {
      await window.haarness.api(connection, "updateThread", [selected, key]);
      if (state.connection?.id === connection) await app.refresh();
    }
    if (draftProject() === project) setChoice(key);
    await app.rememberModel(key);
  }
  async function fork(item: T.Item) {
    if (item.data.type !== "assistant_message") return;
    const connection = state.connection!.id;
    const parent = state.selected;
    // Include the selected answer and bind the source before awaiting.
    const value = await window.haarness.api<T.Thread>(
      connection,
      "forkThread",
      [parent, { upToSequence: item.sequence }],
    );
    if (state.connection?.id === connection && state.selected === parent) {
      await app.refresh();
      if (state.connection?.id === connection && state.selected === parent)
        await app.choose(value.id);
    }
  }
  const efforts = () =>
    state.providers
      .find((p) => p.id === selectedModel()?.provider)
      ?.models.find((m) => m.id === selectedModel()?.model)?.reasoningEfforts ??
    [];
  createEffect(() => {
    if (state.ready && effort() && !efforts().includes(effort())) setEffort("");
  });
  const thought = createMemo(() =>
    state.preferences.thinking === "compact" && !stream()?.text
      ? latestThought(stream()?.reasoning ?? "")
      : "",
  );
  const [reviewClock, setReviewClock] = createSignal(0);
  const activity = createMemo(() => {
    const turn = active();
    if (!turn) return "";
    const items = state.items.filter((item) => item.turnId === turn.id);
    const tools = items.filter(
      (item) =>
        item.data.type === "tool_call" &&
        !items.some(
          (result) =>
            result.data.type === "tool_result" &&
            result.data.call_id === (item.data as { call_id: string }).call_id,
        ),
    );
    const tool = tools.at(-1)?.data as
      | Extract<T.ItemData, { type: "tool_call" }>
      | undefined;
    // Auto's reviewer deciding a request: said only once it takes a moment,
    // so a quick review never flashes a word.
    const review = state.reviews?.[turn.id];
    if (review && reviewClock() - review.at >= REVIEW_NOTICE_MS)
      return t("Reviewing…");
    if (tool) return toolActivity(tool.name, tool.arguments);
    return stream()?.text ? t("Writing a reply") : t("Thinking");
  });
  // In Show, a thought being written says Thinking above its text, so the
  // activity line keeps its place (and its word for screen readers) without
  // saying it a second time.
  const thinkingShown = () =>
    !!stream()?.reasoning &&
    !stream()?.text &&
    state.preferences.thinking === "show" &&
    activity() === t("Thinking");
  // Re-read the activity when a running review reaches the notice delay.
  createEffect(() => {
    const turn = active();
    const review = turn ? state.reviews?.[turn.id] : undefined;
    if (!review) return;
    const wait = REVIEW_NOTICE_MS - (Date.now() - review.at);
    if (wait <= 0) return setReviewClock(Date.now());
    const timer = setTimeout(() => setReviewClock(Date.now()), wait);
    onCleanup(() => clearTimeout(timer));
  });
  /**
   * Where the next message will run. Each choice says which checkout it is and
   * where that checkout lives, because "main" and "feature-x" alone do not tell
   * a reader whose files are about to change.
   */
  const locationChoices = createMemo<MenuAction[]>(() => [
    ...(group()?.locations ?? [])
      .filter((location) => location.checkout?.exists !== false)
      .map((location) => ({
        label: t(locationName(location)),
        detail:
          (isWorktree(location) ? t("Worktree") : t("Project folder")) +
          " · " +
          shortPath(location.workspace),
        icon: isWorktree(location) ? ("branch" as const) : ("folder" as const),
        run: () =>
          void app.run(() => app.connect({ recent: location.id }, true)),
      })),
    {
      label: t("New worktree…"),
      detail: t("Create another checkout of this project"),
      icon: "plus" as const,
      run: () => void app.run(() => createWorktree(app)),
    },
    {
      label: t("Show worktrees in sidebar"),
      icon: "settings" as const,
      run: () => void app.run(() => app.setWorktreeView(group()!.id, true)),
    },
  ]);
  const empty = () =>
    !state.selected &&
    editable() &&
    !hasForkOrigin() &&
    !rows().length &&
    !active() &&
    !stream()?.text &&
    !stream()?.reasoning &&
    !pending();
  return (
    <section
      ref={(element) => {
        conversation = element;
        fileDrop.ref(element);
      }}
      class="conversation"
      classList={{
        "empty-start": empty(),
        "file-drop-active": fileDrop.dragging(),
        // Room for the outline from the first prompt, so text never moves when it appears.
        "with-outline": !empty(),
      }}
      aria-label={t("Chat")}
    >
      <Show when={fileDrop.dragging()}>
        <div class="chat-file-drop-hint" role="status">{t("Drop files to attach")}</div>
      </Show>
      <Show when={findOpen()}>
        <ConversationFind
          rows={rows()}
          items={retainedItems(state.items, thread())}
          reveal={revealFind}
          selected={setFindMatch}
          close={closeFind}
        />
      </Show>
      <Show when={thread()?.revert && editable()}>
        <div class="conversation-history-notice" role="status">
          <span>
            {t("Turn undone. Sending a new message replaces the undone history.")}
          </span>
          <button
            disabled={controls.disabled("redo")}
            onClick={() => app.run(() => controls.invoke("redo"))}
          >
            {t("Redo turn")}
          </button>
        </div>
      </Show>
      <Show when={controls.busy()}>
        <div class="conversation-history-notice" role="status">
          {t("Updating chat history…")}
        </div>
      </Show>
      <Show when={archived()}>
        <div class="conversation-history-notice archived-notice" role="status">
          <Icon name="archive" size="sm" />
          <span>{t("This chat is archived. Sending a message restores it.")}</span>
          <button onClick={() => app.run(() => restore())}>
            {t("Restore")}
          </button>
        </div>
      </Show>
      <div class="transcript-frame">
        <div
          class="transcript"
          ref={scrolling}
          tabIndex={0}
          aria-label={t("Chat history")}
          onScroll={onScroll}
          onFocusIn={onHistoryFocus}
          onWheel={(event) => {
            if (event.deltaY < 0) pauseFollowing();
          }}
          onTouchStart={(event) => {
            touchY = event.touches[0]?.clientY ?? 0;
          }}
          onTouchMove={(event) => {
            const next = event.touches[0]?.clientY ?? touchY;
            if (next > touchY) pauseFollowing();
            touchY = next;
          }}
          onKeyDown={(event) => {
            if (
              ["ArrowUp", "PageUp", "Home"].includes(event.key) ||
              (event.key === " " && event.shiftKey)
            )
              pauseFollowing();
            // End means the latest output, like Jump to latest. The native
            // scroll still runs: it takes over a PageUp scroll in flight,
            // which a scroll set from here would not stop.
            else if (
              event.key === "End" &&
              !(event.target as HTMLElement).closest(
                "input, textarea, select, [contenteditable]",
              )
            ) {
              setFollow(true);
              settleAtEnd = true;
            }
          }}
          onPointerDown={(event) => {
            // A drag that selects text can scroll; End no longer owns that.
            settleAtEnd = false;
            // Include overlay scrollbars, which may report a zero-width gutter.
            if (event.clientX >= scrolling.getBoundingClientRect().right - 16)
              pauseFollowing();
          }}
        >
          <Show when={empty()}>
            <div class="conversation-empty">
              <div class="start-mark" aria-hidden="true">
                <Mark />
              </div>
              <h2>
                <Show
                  when={project()}
                  fallback={projectHeading(t("{project} to get started"))}
                >
                  {projectHeading(t("What should we work on in {project}?"))}
                </Show>
              </h2>
              <Show when={project()}>
                <div class="suggestions" aria-label={t("Chat starters")}>
                  <For each={projectStarters}>
                    {(item) => (
                      <button
                        aria-label={t(item.label)}
                        onClick={() => {
                          change(t(item.text));
                          composer.focus();
                        }}
                      >
                        <span class="starter-icon" aria-hidden="true">
                          <Icon name={item.icon} size="md" />
                        </span>
                        <span class="starter-copy">
                          <strong>{t(item.label)}</strong>
                          <span>{t(item.detail)}</span>
                        </span>
                      </button>
                    )}
                  </For>
                </div>
              </Show>
              <Show when={project() && !localHintDismissed() && localModel()}>
                {(local) => (
                  <p class="local-model-hint">
                    <Icon name="computer" size="sm" />
                    <span>
                      {t("Replies come from {model}, which {runtime} runs on this computer.", {
                        model: local().model,
                        runtime: local().runtime,
                      })}
                    </span>
                    <button
                      type="button"
                      class="link"
                      onClick={() => app.openConnect()}
                    >
                      {t("Connect a provider for larger models")}
                    </button>
                    <Button
                      icon="close"
                      size="sm"
                      label={t("Dismiss")}
                      onClick={dismissLocalHint}
                    />
                  </p>
                )}
              </Show>
            </div>
          </Show>
          <div
            class="transcript-body"
            style={{ height: virtual.getTotalSize() + "px" }}
          >
            <For each={virtual.getVirtualItems()}>
              {(row) => (
                <div
                  ref={(element) =>
                    measureRow(element, virtual.measureElement)
                  }
                  data-index={row.index}
                  data-item-id={rows()[row.index]?.id}
                  classList={{
                    "conversation-match": rows()[row.index]?.id === findMatch(),
                    "fork-source": !!sourceReply() && rows()[row.index]?.id === sourceReply(),
                  }}
                  style={{
                    position: "absolute",
                    top: 0,
                    left: 0,
                    width: "100%",
                    transform: `translateY(${row.start}px)`,
                  }}
                >
                  <Show
                    when={hasForkOrigin() && forkDividerIndex() === row.index}
                  >
                    <ForkDivider />
                  </Show>
                  <Message
                    item={rows()[row.index]}
                    matched={rows()[row.index]?.id === findMatch()}
                    task={(() => {
                      const item = rows()[row.index];
                      return item && "call_id" in item.data
                        ? tasks().get(taskKey(item.turnId, item.data.call_id))
                        : undefined;
                    })()}
                    result={toolResult(
                      state.items,
                      rows()[row.index],
                      thread()?.revert?.sequence,
                    )}
                    reply={reply(rows()[row.index])}
                    fork={() => app.run(() => fork(rows()[row.index]))}
                  />
                </div>
              )}
            </For>
          </div>
          <Show when={hasForkOrigin() && forkDividerIndex() === -1}>
            <ForkDivider />
          </Show>
          <Show when={stream()?.reasoning && state.preferences.thinking === "show"}>
            <div class="message reasoning">
              <div class="message-label">
                {/* While it is the work under way, its label is the live one. */}
                <Shimmer class="author" active={thinkingShown()}>
                  {t("Thinking")}
                </Shimmer>
              </div>
              <Markdown text={stream()!.reasoning} live />
            </div>
          </Show>
          <Show when={stream()?.text}>
            {(() => {
              let reply!: HTMLDivElement;
              // Read untracked: Show draws this once per live reply.
              const turn = untrack(() => active()?.id);
              const saved = () =>
                state.items.filter(
                  (item) =>
                    item.turnId === turn &&
                    item.data.type === "assistant_message",
                );
              const earlier = new Set(untrack(saved).map((item) => item.id));
              // The live reply goes when its text is saved as a message of
              // the turn: that message keeps what the person selected.
              onCleanup(() =>
                replySelection.keep(
                  reply,
                  saved().findLast((item) => !earlier.has(item.id))?.id,
                ),
              );
              return (
                <div
                  ref={reply}
                  class="message assistant"
                  data-testid="streaming-reply"
                >
                  <Markdown text={stream()!.text} live />
                </div>
              );
            })()}
          </Show>
          <Show when={active() && !pending()}>
            <div
              class="assistant-activity"
              classList={{ "activity-said": thinkingShown() }}
              role="status"
              aria-live="polite"
            >
              <Shimmer active={!thinkingShown()}>{activity()}</Shimmer>
              {/* Compact: the newest line of thought ticks here, on the one
                  row already holding the activity, so nothing moves; screen
                  readers hear only the activity. */}
              <Show when={thought()}>
                <ThoughtTicker text={thought()} />
              </Show>
            </div>
          </Show>
          <div class="transcript-tail" />
        </div>
        {/* Keyed by conversation, so an open panel never carries over. */}
        <Show when={!empty() && outline().length > 1 ? state.selected : undefined} keyed>
          <ConversationOutline
            prompts={outline}
            reading={readingPrompt}
            height={laneHeight}
            onJump={jumpToPrompt}
            onWheel={outlineWheel}
          />
        </Show>
      </div>
      <Show when={editable() || pending() || !follow() || todos().length}>
        <div class="composer-area" ref={toastAnchor("composer")}>
          <Show when={empty()}>
            <div class="composer-project">
              <ProjectPicker />
              <Show when={group()?.git && !group()?.remote}>
                <MenuButton
                  label={t("Choose working location")}
                  icon="branch"
                  class="location-picker"
                  text={t(locationName(project()))}
                  title={project()?.workspace}
                  items={locationChoices()}
                />
              </Show>
            </div>
          </Show>
          <Show when={!follow()}>
            <button
              class="jump-latest"
              aria-label={t("Jump to latest")}
              title={t("Jump to latest")}
              onClick={() => {
                toggled = undefined;
                setFollow(true);
                followLatest();
              }}
            >
              <Icon name="jump" size="sm" />
            </button>
          </Show>
          <Show when={pending()}>
            <div class="approvals">
              <Show when={currentPermission()} keyed>
                {(permission) => (
                  <Permission
                    permission={permission}
                    review={currentReview()}
                    queued={visiblePermissions().length}
                  />
                )}
              </Show>
              <Show when={currentQuestion()} keyed>
                {(question) => <Question question={question} />}
              </Show>
            </div>
          </Show>
          <span class="sr-only" role="status">
            {active()
              ? pending()
                ? t("Waiting for your response")
                : t("Haarness is working")
              : ""}
          </span>
          {/* One dock at a time: a waiting decision takes the plan's place and
              the plan returns once the queue is empty. */}
          <Show when={todos().length && !pending()}>
            <PlanDock
              id={plan()!.id}
              items={todos()}
              scope={(state.connection?.id ?? "") + ":" + state.selected}
              running={!!active()}
              onDismiss={() => {
                const target = editable() ? composer : scrolling;
                if (target?.isConnected) target.focus({ preventScroll: true });
              }}
            />
          </Show>
          <Show when={editable()}>
            <Show when={voice.error()}>
              <div class="dictation-error" role="alert">
                <span>{voice.error()}</span>
                <Button
                  icon="close"
                  label={t("Dismiss dictation error")}
                  onClick={voice.clearError}
                />
              </div>
            </Show>
            <Show when={state.commandNotes[commandNoteKey()]} keyed>
              {(note) => (
                <div class="command-note">
                  <header>
                    <Icon name="terminal" size="sm" />
                    {/* The command and the server's answer are usually English;
                        each takes its own direction in a right-to-left window. */}
                    <span
                      class="command-note-title"
                      title={note.title}
                      dir="auto"
                    >
                      {note.title}
                    </span>
                    <Button
                      icon="close"
                      size="sm"
                      class="command-note-dismiss"
                      label={t("Dismiss")}
                      onClick={() =>
                        set("commandNotes", commandNoteKey(), undefined)
                      }
                    />
                  </header>
                  <p class="command-note-text" role="status" dir="auto">
                    {note.text}
                  </p>
                </div>
              )}
            </Show>
            <InputQueue queue={queue} working={!!active()} />
            <div
              class="composer"
              data-agent={selectedAgent()}
              style={{
                "--composer-agent": agentColor(selectedAgent()),
              }}
              classList={{ running: !!active(), waiting: pending() }}
              onPaste={(event) => {
                const files = Array.from(event.clipboardData?.files ?? []);
                if (!files.length) {
                  if (
                    event.target === composer &&
                    foldPaste(event.clipboardData?.getData("text/plain") ?? "")
                  )
                    event.preventDefault();
                  return;
                }
                event.preventDefault();
                const uris = event.clipboardData?.getData("text/uri-list");
                void app.run(() => receiveFiles(files, uris));
              }}
            >
              <Show when={attachments().length}>
                <div class="composer-attachments">
                  <For each={attachments()}>
                    {(item, index) => (
                      <span class="attachment-chip">
                        <Show
                          when={item.data.startsWith("data:image/")}
                          fallback={<Icon name="file" size="sm" />}
                        >
                          <img src={item.data} alt={item.name} />
                        </Show>
                        <span class="attachment-name">{item.name}</span>
                        <button
                          aria-label={t("Remove {name}", { name: item.name })}
                          // Focus stays in the message box, so the next paste
                          // still reaches it.
                          onMouseDown={(event) => event.preventDefault()}
                          onClick={() => {
                            void setAttachments((items) =>
                              items.filter((_, i) => i !== index()),
                            );
                            // Removed from the keyboard, the button would take
                            // the focus with it.
                            if (document.activeElement !== composer)
                              composer.focus({ preventScroll: true });
                          }}
                        >
                          <Icon name="close" size="xs" />
                        </button>
                      </span>
                    )}
                  </For>
                </div>
              </Show>
              <textarea
                ref={composer}
                data-composer-input=""
                aria-label={t("Message")}
                aria-autocomplete="list"
                aria-controls={slashOpen() ? "slash-commands" : undefined}
                aria-activedescendant={
                  slashOpen() ? "slash-command-" + slashIndex() : undefined
                }
                // Not while a message is being sent: what is typed then is
                // the next message (`send` waits for the first).
                disabled={
                  controls.busy() ||
                  !project() ||
                  !state.ready ||
                  !!state.openingProject ||
                  state.switching
                }
                placeholder={
                  !project()
                    ? t("Choose a project to start a chat")
                    : pickedElements()
                      ? pickedElements() > 1
                        ? t("Ask about these elements…")
                        : t("Ask about this element…")
                      : active()
                        ? t("Send a follow-up message…")
                        : state.selected
                          ? t("Reply, or @ to add files and agents")
                          : t("Do anything, or @ to add files and agents")
                }
                value={draft()}
                onInput={(event) => change(event.currentTarget.value)}
                onCompositionStart={() => (composingText = true)}
                onCompositionEnd={compositionEnded}
                onBlur={() => {
                  if (composingText) compositionEnded();
                }}
                onKeyDown={(event) => {
                  if (event.isComposing || event.defaultPrevented) return;
                  if (event.key === "Enter" && event.altKey && !event.shiftKey && state.connection?.info.capabilities.includes("input-queue.v1")) {
                    event.preventDefault();
                    if (!event.repeat) void app.run(() => send("queue"));
                    return;
                  }
                  if (slashOpen() && !event.isComposing) {
                    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                      event.preventDefault();
                      setSlashIndex(
                        (index) =>
                          (index +
                            (event.key === "ArrowDown" ? 1 : -1) +
                            slashCommands().length) %
                          slashCommands().length,
                      );
                      return;
                    }
                    if (
                      (event.key === "Enter" || event.key === "Tab") &&
                      !event.shiftKey &&
                      !event.metaKey &&
                      !event.ctrlKey
                    ) {
                      event.preventDefault();
                      chooseCommand(slashIndex());
                      return;
                    }
                    if (event.key === "Escape") {
                      event.preventDefault();
                      event.stopPropagation();
                      setSlashDismissed(true);
                      return;
                    }
                  }
                  if (agentKey(event)) return;
                  // Leaving a queued-message edit never stops the running work.
                  if (event.key === "Escape" && queue.editing()) {
                    event.preventDefault();
                    event.stopPropagation();
                    queue.cancel();
                    return;
                  }
                  // Esc in an empty message box stops the work, as Stop
                  // does; a menu or popover of the box, Find, or the
                  // sidebar's chat selection takes it first.
                  if (
                    event.key === "Escape" &&
                    !event.shiftKey &&
                    !event.altKey &&
                    !event.metaKey &&
                    !event.ctrlKey &&
                    showStop() &&
                    !findOpen() &&
                    !chatSelection()
                  ) {
                    event.preventDefault();
                    if (!event.repeat && !stopping()) void app.run(stop);
                    return;
                  }
                  if (
                    ["ArrowUp", "ArrowDown"].includes(event.key) &&
                    !event.altKey &&
                    !event.ctrlKey &&
                    !event.metaKey &&
                    !event.shiftKey
                  ) {
                    if (recall(event.key === "ArrowUp" ? "up" : "down"))
                      event.preventDefault();
                    return;
                  }
                  if (
                    event.key === "Enter" &&
                    !event.isComposing &&
                    !event.shiftKey &&
                    ((state.preferences.sendKey === "enter" &&
                      !event.metaKey &&
                      !event.ctrlKey) ||
                      (state.preferences.sendKey === "mod-enter" &&
                        (event.metaKey || event.ctrlKey)))
                  ) {
                    event.preventDefault();
                    if (event.repeat) return;
                    void app.run(send);
                  }
                }}
              />
              <PromptContextControls
                text={draft()}
                textarea={() => composer}
                change={change}
                insert={insertText}
                disabled={submitting() || stopping() || !state.ready}
                // A queued message's edit saves text, so no new resource.
                resources={!queue.editing()}
                // The agent the next message runs under decides, as for the
                // message itself.
                chat={state.selected ? { threadId: state.selected, agent: selectedAgent() } : { agent: selectedAgent() }}
              />
              <Show when={slashOpen()}>
                <div
                  id="slash-commands"
                  class="slash-suggestions popover"
                  role="listbox"
                  aria-label={t("Slash commands")}
                >
                  <div class="slash-suggestions-header">
                    {t("Commands")}<span>{t("↑ ↓ to browse · Enter to select")}</span>
                  </div>
                  <For each={slashCommands()}>
                    {(command, index) => (
                      <button
                        id={"slash-command-" + index()}
                        role="option"
                        aria-selected={slashIndex() === index()}
                        tabIndex={-1}
                        title={
                          "/" +
                          command.name +
                          (command.description
                            ? " — " + t(command.description)
                            : "")
                        }
                        onPointerMove={() => setSlashIndex(index())}
                        onMouseDown={(event) => event.preventDefault()}
                        onClick={() => chooseCommand(index())}
                      >
                        <strong>/{command.name}</strong>
                        {/* Built-in descriptions are English keys; a project's own pass through unchanged. */}
                        <span>{t(command.description)}</span>
                      </button>
                    )}
                  </For>
                </div>
              </Show>
              <footer>
                <div class="composer-leading">
                  <MenuButton
                    label={t("Attach")}
                    icon="plus"
                    disabled={!project()}
                    // Main lists screenshots for a choice in this menu only.
                    onOpen={() => {
                      screenshotMenu = window.haarness.screenshotMenu().catch(() => null);
                    }}
                    items={[
                      {
                        label: t("Attach files"),
                        icon: "attach",
                        run: () =>
                          void app.run(async () => {
                            const key = app.draftKey();
                            const values = await window.haarness.attachments();
                            await app.addAttachments(key, relabelAttachments(values));
                          }),
                      },
                      {
                        label: t("Attach screenshots"),
                        icon: "image",
                        run: () => {
                          setScreenshotsFor({ key: app.draftKey(), token: screenshotMenu ?? Promise.resolve(null) });
                          screenshotMenu = undefined;
                        },
                      },
                    ]}
                  />
                  {/* In the body, so the composer's own styles stay off the dialog. */}
                  <Show when={screenshotsFor()}>
                    {(open) => (
                      <Portal>
                        <ScreenshotPicker
                          token={open().token}
                          close={(values) => {
                            // Read before closing: the dialog's draft key goes with it.
                            const target = open().key;
                            setScreenshotsFor(undefined);
                            if (values?.length)
                              void app.run(() => app.addAttachments(target, relabelAttachments(values)));
                            if (composer?.isConnected && !composer.disabled)
                              composer.focus({ preventScroll: true });
                          }}
                        />
                      </Portal>
                    )}
                  </Show>
                  <Show when={state.connection}>
                    {/* The name carries the mode it shows, so a voice user
                        can say what they see (WCAG 2.5.3); a name that changes
                        with its state cannot also be a pressed toggle. */}
                    <button
                      class="pill-select approval-mode"
                      classList={{ enabled: app.isAutoApproval() }}
                      disabled={!!state.openingProject}
                      aria-label={t("Approvals: {mode}", {
                        mode: app.isAutoApproval() ? t("Auto") : t("Review"),
                      })}
                      aria-description={approvalModeHint()}
                      title={approvalModeHint()}
                      onClick={() =>
                        app.run(() =>
                          app.setAutoApproval(!app.isAutoApproval()),
                        )
                      }
                    >
                      <Icon name="shield" size="sm" />
                      {/* The same two names as the TUI and the docs. */}
                      {app.isAutoApproval() ? t("Auto") : t("Review")}
                    </button>
                  </Show>
                  <Show when={remoteShared()}>
                    <button
                      class="pill-select remote-pill"
                      aria-label={t("Remote control")}
                      title={t("Live in Haarness Cloud")}
                      onClick={() => app.openRemoteControl("view")}
                    >
                      <span class="remote-dot" aria-hidden="true" />
                      {t("Remote")}
                    </button>
                  </Show>
                </div>
                <div class="composer-trailing">
                  <Show when={state.connection}>
                    <Select
                      class="pill-select agent-select"
                      disabled={controls.busy()}
                      aria-label={t("Agent")}
                      value={selectedAgent()}
                      onChange={(value) => chooseAgent(value)}
                      options={conversationAgents(state.agents).map(
                        (agent) => ({
                          value: agent.id,
                          label: agent.id,
                        }),
                      )}
                    />
                    <span class="sr-only" role="status">
                      {cycledAgent() && t("Agent: {agent}", { agent: cycledAgent() })}
                    </span>
                    <Show
                      when={!state.catalogLoading && selectedModel()}
                      fallback={
                        <button
                          class="pill-select composer-model"
                          disabled={!state.ready || state.catalogLoading}
                          onClick={() => app.openConnect({ onboarding: true })}
                        >
                          {state.ready && !state.catalogLoading
                            ? t("Connect a provider")
                            : t("Loading models…")}
                        </button>
                      }
                    >
                      <ModelPicker
                        class="pill-select composer-model"
                        aria-label={t("Model")}
                        value={modelValue()}
                        disabled={!state.ready || controls.busy()}
                        onChange={(value) => app.run(() => model(value))}
                        options={modelOptions()}
                        effort={effort()}
                        onEffortChange={setEffort}
                        onOpen={() => void app.run(() => app.catalogs(true))}
                      />
                    </Show>
                  </Show>

                  <Show when={state.platform === "darwin"}>
                    <Show when={voice.active()}>
                      <span class="dictation-status" role="status">
                        {voice.status() === "starting"
                          ? voice.preparation()
                          : voice.status() === "finishing"
                            ? "Finishing…"
                            : "Listening…"}
                      </span>
                    </Show>
                    <button
                      type="button"
                      class="icon-button dictation-button"
                      classList={{ recording: voice.active() }}
                      aria-label={
                        voice.active() ? t("Stop dictation") : t("Start dictation")
                      }
                      title={
                        voice.active()
                          ? t("Stop dictation")
                          : `${t("Start dictation")} (${shortcutText("Mod+Shift+M", state.platform)})`
                      }
                      aria-pressed={voice.active()}
                      disabled={
                        !project() ||
                        submitting() ||
                        voice.status() === "finishing"
                      }
                      onPointerDown={(event) => event.preventDefault()}
                      onClick={() => void voice.toggle()}
                    >
                      <Icon name="mic" size="lg" />
                    </button>
                  </Show>
                  <button
                    class="send-button"
                    aria-label={sendLabel()}
                    title={sendHint()}
                    disabled={
                      !project() ||
                      submitting() ||
                      controls.busy() ||
                      stopping() ||
                      (!showStop() && (!state.ready || state.switching)) ||
                      (!showStop() &&
                        !draft().trim() &&
                        !attachments().length &&
                        !app.promptContext().length)
                    }
                    onClick={() => app.run(showStop() ? stop : () => send())}
                  >
                    <Icon
                      name={queue.editing() ? "check" : showStop() ? "stop" : "send"}
                      size="sm"
                      filled={showStop()}
                    />
                  </button>
                </div>
              </footer>
            </div>
          </Show>
        </div>
      </Show>
    </section>
  );
}
/**
 * One durable transcript item.
 *
 * Assistant prose carries no author heading: the reader already knows who is
 * answering, and a repeated name turns a conversation into a log. Identity is
 * carried by position and shape instead — the user speaks in a bubble on the
 * right, Haarness answers in the reading column, and everything the engine
 * *did* sits on a single quiet rail beneath the answer.
 */
/** The new-chat heading with the project picker in it. The word or mark
 * right after the name ("?") stays on its line: a long name gives way with
 * an ellipsis instead of leaving the mark alone on the next line. */
function projectHeading(template: string) {
  const [before, after = ""] = template.split("{project}");
  const [, glued = "", rest = ""] = /^(\S*)([\s\S]*)$/.exec(after) ?? [];
  return (
    <>
      {before}
      <span class="heading-project">
        <ProjectPicker heading />
        {glued}
      </span>
      {rest}
    </>
  );
}
/** A message's time as it shows (short), with the full date and time on
 * hover and for assistive technology. */
function Timestamp(props: { value: string }) {
  const date = () => new Date(props.value);
  const valid = () => !Number.isNaN(date().getTime());
  return (
    <time
      class="message-time"
      dateTime={valid() ? date().toISOString() : undefined}
      title={valid() ? date().toLocaleString(language(), { dateStyle: "full", timeStyle: "short" }) : undefined}
    >
      {time(props.value)}
    </time>
  );
}
/** What the person reads when a provider refused images that later requests leave out. */
function mediaOmittedNotice(names: string[], guessed: boolean) {
  const list = names.join(", ");
  if (!guessed)
    return t("The provider refused {name}, so it is left out of this chat from now on.", { name: list });
  return names.length === 1
    ? t("The provider refused an image without saying which, so the newest one, {name}, is left out of this chat from now on.", { name: list })
    : t("The provider refused an image without saying which, so the newest ones, {names}, are left out of this chat from now on.", { names: list });
}

function Message(props: {
  item: T.Item;
  result?: T.Item;
  task?: Task;
  fork: () => Promise<unknown>;
  /** The message is the current Find match. */
  matched?: boolean;
  /** For a reply: whether it is its turn's last text, which carries the
   * turn's footer, and the turn's whole reply, which Copy takes. */
  reply?: { last: boolean; text: string };
}) {
  const app = useApp();
  const content = () => props.item?.data;
  const kind = () => {
    const type = content().type;
    return type === "user_message"
      ? "user"
      : type === "reasoning"
        ? // A compact thought is a step on the rail, like a tool.
          app.state.preferences.thinking === "compact"
          ? "step"
          : "reasoning"
        : props.task || type === "tool_call" || type === "compaction"
          ? "step"
          : "assistant";
  };
  /**
   * A slash command whose turn is still starting has not expanded yet: its
   * "Show prompt" row is held at the size of the command's template, so it
   * does not appear under the reader a moment later. Undefined once it has
   * expanded, after its turn, or for a template that is only its arguments.
   */
  const pendingPromptLines = () => {
    const data = content();
    if (data.type !== "user_message" || !data.command_line) return undefined;
    if (commandPrompt(data.command_line, data.text) !== undefined) return undefined;
    if (!app.state.turns.some((turn) => turn.id === props.item.turnId)) return undefined;
    const name = /^\/(\S+)/.exec(data.command_line)?.[1]?.toLowerCase();
    const template = app.state.commands.find((command) => command.name.toLowerCase() === name)?.template ?? "";
    if (!template.replace(/\$(?:ARGUMENTS|\d+)/g, "").trim()) return undefined;
    return Math.max(1, countLines(template.trim()));
  };
  const [copied, setCopied] = createSignal(false);
  let copiedTimer: ReturnType<typeof setTimeout>;
  const [forking, setForking] = createSignal(false);
  onCleanup(() => clearTimeout(copiedTimer));
  async function copy(text: string) {
    await navigator.clipboard.writeText(text);
    setCopied(true);
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => setCopied(false), 1600);
  }
  /**
   * Copy and Fork stay out of the reading rhythm until the pointer or the
   * keyboard reaches this message. Their row keeps its height either way, so
   * revealing them never moves the transcript.
   */
  const MessageTools = () => (
    <div class="message-tools">
      <button
        class="icon-button"
        aria-label={copied() ? t("Copied") : t("Copy message")}
        title={copied() ? t("Copied") : t("Copy message")}
        onClick={() =>
          app.run(() => {
            const data = content();
            // A slash command copies the line typed, a message with pastes
            // the text pasted, as the bubble shows them.
            return copy(
              data.type === "user_message"
                ? plainMessage(data.command_line || data.text)
                : (props.reply?.text ?? (data as { text: string }).text),
            );
          })
        }
      >
        <Icon name={copied() ? "check" : "copy"} size="sm" />
      </button>
      <Show when={content().type === "assistant_message"}>
        <button
          class="icon-button"
          aria-label={t("Fork from message")}
          title={t("Fork from message")}
          disabled={forking()}
          onClick={async () => {
            if (forking()) return;
            setForking(true);
            try {
              await props.fork();
            } finally {
              setForking(false);
            }
          }}
        >
          <Icon name="fork" size="sm" />
        </button>
      </Show>
    </div>
  );
  return (
    <Show when={props.item}>
      <article class={"message " + kind()} data-type={content().type}>
        <Switch>
          <Match when={props.task}>
            <AgentTask task={props.task!} />
          </Match>
          <Match when={content().type === "user_message"}>
            <div class="bubble">
              <UserMessageBody
                id={props.item.id}
                text={(content() as { text: string }).text}
                commandLine={
                  (content() as Extract<T.ItemData, { type: "user_message" }>)
                    .command_line
                }
                revealed={props.matched}
                pendingPromptLines={pendingPromptLines()}
              >
                <For
                  each={
                    (content() as Extract<T.ItemData, { type: "user_message" }>)
                      .attachments
                  }
                >
                  {(attachment) =>
                    attachment.startsWith("data:image/") ? (
                      <span
                        class="attachment-chip attachment-image"
                        ref={(element) => {
                          // Shown smaller than it is, an attached image opens full size.
                          const opening = watchOpenImages(element, () => t("Attached image"));
                          onCleanup(opening.dispose);
                        }}
                      >
                        <img src={attachment} alt={t("Attached image")} />
                      </span>
                    ) : (
                      <span class="attachment-chip">
                        <Icon name="file" size="sm" />
                        <span class="attachment-name">
                          {attachmentName(attachment)}
                        </span>
                      </span>
                    )
                  }
                </For>
              </UserMessageBody>
            </div>
            <div class="message-footer">
              <Timestamp value={props.item.createdAt} />
              <MessageTools />
            </div>
          </Match>

          <Match when={content().type === "assistant_message"}>
            <Markdown text={(content() as { text: string }).text} />
            <div class="message-footer">
              <Show when={props.reply?.last ?? true}>
                <Timestamp value={props.item.createdAt} />
                <MessageTools />
              </Show>
            </div>
          </Match>

          <Match
            when={
              content().type === "reasoning" &&
              app.state.preferences.thinking === "compact"
            }
          >
            <ThoughtRow
              id={props.item.id}
              matched={props.matched}
              text={(content() as { text: string }).text}
              durationMs={
                (content() as Extract<T.ItemData, { type: "reasoning" }>)
                  .duration_ms
              }
            />
          </Match>
          <Match when={content().type === "reasoning"}>
            <div class="message-label">
              <span class="author">{t("Thinking")}</span>
              <Timestamp value={props.item.createdAt} />
              <MessageTools />
            </div>
            <Markdown text={(content() as { text: string }).text} />
          </Match>

          <Match when={content().type === "tool_call"}>
            {(() => {
              const tool = () =>
                content() as Extract<T.ItemData, { type: "tool_call" }>;
              const result = () =>
                props.result?.data as
                  | Extract<T.ItemData, { type: "tool_result" }>
                  | undefined;
              return (
                <ToolActivity
                  name={tool().name}
                  args={tool().arguments}
                  result={result()}
                  id={props.item.id}
                  expanded={!app.state.preferences.compactTools}
                  waiting={app.state.permissions.some(
                    (permission) =>
                      permission.threadId === props.item.threadId &&
                      !!props.item.turnId &&
                      permission.turnId === props.item.turnId,
                  )}
                />
              );
            })()}
          </Match>

          <Match when={content().type === "error"}>
            {(() => {
              const message = () => (content() as { message: string }).message;
              const recovery = () => errorRecovery(message());
              // The user's own Stop is not a failure; it reads as a quiet note.
              if (message().trim() === "operation cancelled")
                return (
                  <div class="notice-inline turn-stopped" role="status">
                    <Icon name="stop" size="sm" />
                    <span>{t("Stopped")}</span>
                  </div>
                );
              return (
                <div class="inline-error" role="alert">
                  <Icon name="error" size="sm" />
                  <span>{message()}</span>
                  <Show when={recovery()}>
                    <button
                      class="inline-error-action"
                      onClick={() => app.set("settings", recovery()!.pane)}
                    >
                      {t(recovery()!.label)}
                    </button>
                  </Show>
                </div>
              );
            })()}
          </Match>

          <Match when={content().type === "media_omitted"}>
            {(() => {
              const omitted = () =>
                content() as Extract<T.ItemData, { type: "media_omitted" }>;
              return (
                <div class="notice-inline media-omitted" role="status">
                  <Icon name="alert" size="sm" />
                  <span>
                    {mediaOmittedNotice(omitted().names, !!omitted().guessed)}
                    <Show when={omitted().reason}>
                      <small class="notice-detail">{omitted().reason}</small>
                    </Show>
                  </span>
                </div>
              );
            })()}
          </Match>

          <Match when={content().type === "plan"}>
            <div class="plan-block">
              <span class="block-label">
                <Icon name="book" size="xs" />
                {t("Plan")}
              </span>
              <Markdown text={(content() as { markdown: string }).markdown} />
            </div>
          </Match>

          <Match when={content().type === "review"}>
            <pre>
              {stringify((content() as { findings: unknown }).findings)}
            </pre>
          </Match>

          <Match when={content().type === "compaction"}>
            <details class="tool-call" data-status="done">
              <summary>
                <span class="tool-mark" aria-hidden="true">
                  <Icon name="layers" size="sm" />
                </span>
                <strong>{t("Compacted")}</strong>
                <span class="tool-target">{t("Earlier turns were summarised")}</span>
                <Icon name="chevron" size="xs" class="tool-chevron" />
              </summary>
              <div class="tool-body">
                <Markdown text={(content() as { summary: string }).summary} />
              </div>
            </details>
          </Match>

          <Match when={content().type === "mcp_action"}>
            <div class="activity-note">
              <Icon name="plugin" size="xs" />
              MCP · {(content() as { server: string }).server} ·{" "}
              {(content() as { action: string }).action}
            </div>
          </Match>
        </Switch>
      </article>
    </Show>
  );
}
/**
 * A waiting permission. It belongs to the conversation that raised it and to
 * nothing else: navigation, other projects and settings all stay usable while
 * it waits, and a request raised by a sub-agent links back to its own session.
 */
/** A compact unified diff for one edit inside a transcript step. */

/** Model runtimes Haarness finds on this computer without being told. */
const localRuntimes: Record<string, string> = {
  ollama: "Ollama",
  lmstudio: "LM Studio",
};

const permissionTitles: Record<string, string> = {
  bash: "Run a command",
  read: "Read a file",
  edit: "Edit a file",
  write: "Write a file",
  external_directory: "Access a folder outside this project",
  webfetch: "Open a web page",
  workflow: "Run a workflow",
  agent_switch: "Switch the active agent",
  media_generate: "Generate media",
  mcp_resource_list: "List an MCP server's resources",
  mcp_resource_read: "Read a resource from an MCP server",
  browser: "Use the browser",
  browser_site: "Allow site access",
  multiple: "Allow browser and file access",
  doom_loop: "Repeat this action",
};

/**
 * The one request waiting on the reader. It docks above the composer like
 * the plan, so the transcript stays a record and the decision stays where
 * the next message is typed.
 */
function Permission(props: {
  permission: T.PendingPermissionInfo;
  /** Its review note as announced last (it can change while it is shown). */
  review?: T.PermissionReview | null;
  /** How many requests wait for this conversation, this one included. */
  queued: number;
}) {
  const app = useApp();
  const [answered, setAnswered] = createSignal(false);
  const shape = () =>
    describeTool(props.permission.operation, props.permission.detail);
  const detail = () => {
    const value = props.permission.detail;
    if (props.permission.permission === "agent_switch" && value && typeof value === "object" && !Array.isArray(value)) {
      const switcher = value.__agentSwitch;
      if (switcher && typeof switcher === "object" && !Array.isArray(switcher)) {
        return [
          `Agent: ${switcher.agent}`,
          `Model: ${switcher.model}`,
          switcher.previousModel !== switcher.model ? `Previous model: ${switcher.previousModel}` : undefined,
          typeof value.reason === "string" ? `Reason: ${value.reason}` : undefined,
          "Always allow applies only to this agent in this chat.",
        ].filter(Boolean).join("\n");
      }
    }
    if (!["browser", "browser_site", "multiple"].includes(props.permission.permission ?? "") ||
        !value || typeof value !== "object" || Array.isArray(value)) return stringify(value);
    // Keep the actual script, URL and file paths reviewable; broker leases,
    // document generations and the full policy snapshot are internal metadata.
    return stringify({
      permissions: props.permission.patterns,
      ...Object.fromEntries(Object.entries(value).filter(([key]) => key !== "__browser")),
    });
  };
  let allowOnce: HTMLButtonElement | undefined;
  // Settled already (Auto's reviewer, another window): one quiet line takes
  // the buttons' place, at their height, until the card goes. No toast: the
  // person's answer did nothing, and nothing else needs saying.
  const [settled, setSettled] = createSignal<{ height: number; focused: boolean }>();
  let settledTimer: ReturnType<typeof setTimeout> | undefined;
  let footer: HTMLElement | undefined;
  onCleanup(() => clearTimeout(settledTimer));
  const showSettled = () => {
    // The line holds the buttons' height, a second row of them included,
    // and keeps the keyboard where it was.
    setSettled({
      height: footer?.offsetHeight ?? 0,
      focused: !!footer?.contains(document.activeElement),
    });
    settledTimer = setTimeout(() => void app.refresh(), SETTLED_NOTE_MS);
  };
  const decide = (decision: string) => {
    if (answered()) return;
    const connection = app.state.connection!.id;
    const id = props.permission.permissionId;
    setAnswered(true);
    void app.run(async () => {
      try {
        // Once + Auto still turns Auto on.
        if (decision === "auto") {
          if (await app.allowOnceAndAuto(props.permission)) return showSettled();
        } else
          await app.api("replyPermission", props.permission.permissionId, {
            decision,
          });
        await app.refresh();
      } catch (error) {
        if (alreadySettled(error)) return showSettled();
        setAnswered(false);
        app.recordPermissionError(connection, id, error);
        throw error;
      }
    });
  };
  const category = () =>
    props.permission.permission || props.permission.operation.split(":")[0];
  const title = () => permissionTitles[category()] ?? "Permission needed";
  // A site in the user's own Chrome is answered by a person, for this chat
  // at most: no folder or repository rule, and no switch to Auto.
  const explicit = () => isExplicitPermission(props.permission);
  const otherPatterns = () =>
    (props.permission.patterns ?? []).filter((pattern) => !chromeApproval(pattern));
  const explicitDetail = () => {
    const value = props.permission.detail;
    if (!value || typeof value !== "object" || Array.isArray(value)) return "";
    const rest = Object.entries(value).filter(([key]) => key !== "__browser");
    return rest.length ? stringify(Object.fromEntries(rest)) : "";
  };
  // A path that a link or `..` takes elsewhere is also asked about where it
  // leads; name that file, since it is what gets opened.
  const linkTargets = () => {
    const value = props.permission.detail;
    const path =
      value && typeof value === "object" && !Array.isArray(value) ? value.path : undefined;
    if (typeof path !== "string" || !["read", "edit"].includes(category())) return [];
    return (props.permission.patterns ?? []).filter((pattern) => pattern !== path);
  };
  // A command Haarness could not read all of asks because a rule might be
  // about the part it did not read; the server names that rule, or none when
  // the whole line was too large to read.
  const readLimit = () => {
    const value = props.permission.detail;
    if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
    return typeof value.__readLimit === "string" ? value.__readLimit : undefined;
  };
  // The script the command runs, as the server read it when it asked. Shut
  // until asked for, so a short card stays short, and held open by request
  // so a refresh of the list never closes it under the reader.
  const script = () =>
    permissionScript(props.permission, app.state.connection?.workspace ?? "");
  const scriptKey = () => `permission:${props.permission.permissionId}:script`;
  const scriptBody = `permission-script-${props.permission.permissionId}`;
  // A file change is approved for what it changes, so show the change itself
  // rather than only the path it touches.
  const changes = () =>
    ["edit", "write"].includes(category())
      ? toolChanges(category(), props.permission.detail)
      : [];
  // What "always" would let through, in words rather than as stored rules.
  const alwaysRules = () =>
    (props.permission.alwaysPatterns ?? []).map((rule) =>
      describeAlwaysRule(category(), rule, { t, shortPath }),
    );
  const autoOffered = () => !app.isAutoApproval() && canAutoApprove(props.permission);
  /* Deny, Always and Allow once share one line, even in a narrow chat:
     every way of allowing more than this request waits in the Always menu,
     Allow once + Auto included. */
  const alwaysChoices = (): MenuAction[] => [
    ...(props.permission.allowAlways !== false
      ? [
          {
            label: t("Always for this folder"),
            detail: middleTruncate(
              alwaysRules().join(", ") || shortPath(app.state.connection?.workspace ?? ""),
            ),
            icon: "folder" as const,
            run: () => decide("always_workspace"),
          },
          ...(props.permission.repositoryScope
            ? [
                {
                  label: t("Always for this repository"),
                  detail: middleTruncate(
                    t("Every linked worktree of {repository}", {
                      repository: shortPath(props.permission.repositoryScope),
                    }),
                  ),
                  icon: "branch" as const,
                  run: () => decide("always_repository"),
                },
              ]
            : []),
        ]
      : []),
    ...(autoOffered()
      ? [
          {
            label: t("Allow once + Auto"),
            detail: t("Then Auto decides for this chat"),
            icon: "shield" as const,
            divider: props.permission.allowAlways !== false,
            run: () => decide("auto"),
          },
        ]
      : []),
  ];
  let dock!: HTMLElement;
  const holdKeyboard = cardKeyboard(() => dock);
  return (
    <section
      ref={(element) => {
        dock = element;
        holdKeyboard(element);
      }}
      class="prompt-dock approval"
      tabindex="-1"
      role="alert"
      aria-label={t("Permission request")}
    >
      <header class="prompt-head">
        <Icon name={shape().icon} size="sm" />
        <strong class="prompt-title">
          {explicit() ? t("Allow in Chrome") : t(title())}
        </strong>
        <Show when={props.queued > 1}>
          <span class="prompt-count">
            {t("{index} of {total}", { index: 1, total: props.queued })}
          </span>
        </Show>
        <Show when={props.permission.threadId !== app.state.selected}>
          <button
            class="ghost prompt-link"
            onClick={() => app.run(() => app.choose(props.permission.threadId))}
          >
            {t("View agent chat")}
          </button>
        </Show>
      </header>
      <Show
        when={!explicit()}
        fallback={
          <>
            <ul class="prompt-subject chrome-approval">
              <For each={chromeApprovalLines(props.permission.patterns ?? [])}>
                {(line) => (
                  <li>
                    {splitAround(
                      chromeRequestText(line.category),
                      () => <bdi>{line.origin}</bdi>,
                      "{origin}",
                    )}
                  </li>
                )}
              </For>
              <For each={otherPatterns()}>
                {(pattern) => (
                  <li>
                    <code>{pattern}</code>
                  </li>
                )}
              </For>
            </ul>
            <Show when={explicitDetail()}>
              <pre class="prompt-subject prompt-detail">{explicitDetail()}</pre>
            </Show>
          </>
        }
      >
        <Show
          when={permissionCommand(props.permission.detail)}
          fallback={<pre class="prompt-subject prompt-detail">{detail()}</pre>}
        >
          <pre class="prompt-subject approval-command">
            {permissionCommand(props.permission.detail)}
          </pre>
        </Show>
        {/* Why Auto's reviewer left this to the person: one quiet line under
            the command, there from the card's first paint, above the script. */}
        <Show when={reviewNoteParts(props.review, t)}>
          {(note) => (
            <p class="approval-review">
              {note().lead}
              <Show when={note().rule}>
                {(rule) => (
                  <>
                    {" · "}
                    <bdi>{rule()}</bdi>
                  </>
                )}
              </Show>
              <Show when={note().reason}>
                {(reason) => (
                  <>
                    {": "}
                    <bdi>{reason()}</bdi>
                  </>
                )}
              </Show>
            </p>
          )}
        </Show>
        <Show when={script()?.withheld ? script() : undefined}>
          {(item) => (
            <p class="approval-script-note">
              {splitAround(
                item().withheld === "written"
                  ? t("This command writes {path} before running it, so its contents can't be shown.")
                  : t("An earlier step of this command can change {path}, so its contents aren't shown."),
                () => <bdi><code>{item().path}</code></bdi>,
                "{path}",
              )}
            </p>
          )}
        </Show>
        <Show when={script()?.withheld ? undefined : script()}>
          {(item) => (
            <div class="approval-script" classList={{ open: isOpen(scriptKey()) }}>
              <button
                type="button"
                class="approval-script-toggle"
                aria-expanded={isOpen(scriptKey())}
                aria-controls={scriptBody}
                onClick={() => setOpen(scriptKey(), !isOpen(scriptKey()))}
              >
                <Icon name="chevron" size="xs" class="approval-script-chevron" />
                <span>
                  {isOpen(scriptKey()) ? t("Hide script") : scriptLabel(item())}
                </span>
              </button>
              <Show when={isOpen(scriptKey())}>
                {/* Inside the card's alert, opening it is not news: a screen
                    reader reads it when asked, not 30 KB at once. */}
                <div
                  class="change-preview approval-script-body"
                  id={scriptBody}
                  aria-live="off"
                >
                  <div class="change-path" title={item().path}>
                    <bdi dir="ltr">{item().path}</bdi>
                  </div>
                  <div
                    class="change-lines"
                    tabindex="0"
                    role="region"
                    aria-label={t("Script")}
                  >
                    <For each={item().lines}>
                      {(line) => (
                        <div class="diff-line">
                          <code>{line || " "}</code>
                        </div>
                      )}
                    </For>
                  </div>
                  <Show when={item().truncated}>
                    <p class="change-more">{t("Only the start of the file is shown.")}</p>
                  </Show>
                </div>
              </Show>
            </div>
          )}
        </Show>
        <For each={linkTargets()}>
          {(target) => (
            <p class="approval-target">
              {splitAround(t("Leads to {path}"), () => <bdi><code>{target}</code></bdi>, "{path}")}
            </p>
          )}
        </For>
        <Show when={readLimit() !== undefined}>
          <p class="approval-target">
            <Show
              when={readLimit()}
              fallback={t("Haarness could not read all of this command.")}
            >
              {(rule) =>
                splitAround(
                  t("Haarness could not read all of this command; a rule for {rule} may apply."),
                  () => <bdi><code>{rule()}</code></bdi>,
                  "{rule}",
                )
              }
            </Show>
          </p>
        </Show>
      </Show>
      <Show when={changes().length}>
        <div class="approval-changes">
          <For each={changes()}>
            {(change) => (
              <ChangePreview change={change} showPath={changes().length > 1} />
            )}
          </For>
        </div>
      </Show>
      <Show when={app.permissionError(props.permission.permissionId)}>
        <p class="error" role="alert" data-testid="permission-reply-error">
          {app.permissionError(props.permission.permissionId)}
        </p>
      </Show>
      <Show when={settled()}>
        {(state) => (
          <p
            class="prompt-actions approval-settled"
            role="status"
            tabindex={state().focused ? -1 : undefined}
            style={{ "min-height": state().height ? `${state().height}px` : undefined }}
            ref={(line) => {
              if (state().focused) queueMicrotask(() => line.focus());
            }}
          >
            {t("Already answered")}
          </p>
        )}
      </Show>
      <Show when={explicit() && !settled()}>
        <footer class="prompt-actions" ref={footer}>
          <button
            class="ghost"
            disabled={answered()}
            onClick={() => decide("reject")}
          >
            {t("Deny")}
          </button>
          <button ref={allowOnce} disabled={answered()} onClick={() => decide("once")}>
            {t("Allow once")}
          </button>
          <button
            class="primary"
            disabled={answered()}
            onClick={() => decide("always")}
            title={t("Lasts for this chat until its Haarness server stops.")}
          >
            {t("Allow in this chat")}
          </button>
        </footer>
      </Show>
      <Show when={!explicit() && !settled()}>
        <footer class="prompt-actions" ref={footer}>
          {/* With nothing to allow always, Allow once + Auto keeps a button
              of its own at the start, apart from the answers: when Auto
              turns on and it goes, nothing else moves, and its focus passes
              to Allow once. */}
          <Show when={autoOffered() && props.permission.allowAlways === false}>
            <button
              class="prompt-auto"
              ref={(button) =>
                onCleanup(() => {
                  if (document.activeElement === button)
                    queueMicrotask(() => allowOnce?.focus());
                })
              }
              disabled={answered()}
              onClick={() => decide("auto")}
              title={t("Allow this request once and enable Auto for this chat and its agents")}
            >
              {t("Allow once + Auto")}
            </button>
          </Show>
          <button
            class="ghost"
            disabled={answered()}
            onClick={() => decide("reject")}
          >
            {t("Deny")}
          </button>
          {/* Always a menu, whatever it holds, so the answers keep their
              places when Auto comes or goes. */}
          <Show when={props.permission.allowAlways !== false}>
            <MenuButton
              label={t("Always allow")}
              content={
                <>
                  {t("Always allow")}
                  <Icon name="down" size="xs" />
                </>
              }
              class="prompt-always"
              disabled={answered()}
              title={alwaysRules().join("\n") || t("Always allow")}
              items={alwaysChoices()}
            />
          </Show>
          <button
            class="primary"
            ref={allowOnce}
            disabled={answered()}
            onClick={() => decide("once")}
          >
            {t("Allow once")}
          </button>
        </footer>
      </Show>
    </section>
  );
}
/** A menu line too long for the menu, with its start and its end kept (a
 * command's start, a path's last folders) and the middle left out. */
function middleTruncate(text: string, length = 34) {
  const characters = Array.from(text);
  if (characters.length <= length) return text;
  const head = Math.ceil((length - 1) / 2);
  return characters.slice(0, head).join("") + "…" + characters.slice(characters.length - (length - 1 - head)).join("");
}
/** The closed script toggle: its size, which for a cut file counts only
 * what was read. */
function scriptLabel(script: { lines: string[]; truncated: boolean }) {
  const count = script.lines.length;
  if (script.truncated)
    return count === 1
      ? t("View script · start of the file")
      : t("View script · first {count} lines", { count: formatNumber(count) });
  return count === 1
    ? t("View script · 1 line")
    : t("View script · {count} lines", { count: formatNumber(count) });
}
/** What an explicit Chrome request lets the agent do, with `{origin}` left
 * for the caller to isolate (an origin is a Latin run in any language). */
function chromeRequestText(category: ChromeField) {
  switch (category) {
    case "browsing":
      return t("Use {origin} in Chrome");
    case "downloads":
      return t("Send downloads from {origin} in Chrome to the agent");
    case "uploads":
      return t("Upload files to {origin} in Chrome");
    case "scripts":
      return t("Run scripts on {origin} in Chrome");
  }
}
/** How long a card answered elsewhere says so before it goes. */
const SETTLED_NOTE_MS = 1500;
/**
 * The keyboard around a card docked above the message box. A card never
 * takes the keyboard from where it is: the message box stays in view under
 * it, so what is typed there stays the next message, and Shift+Tab or the
 * pointer reaches the card's answers. Only when nothing has the keyboard, or
 * in an agent's chat, which has no message box, the card itself takes it
 * (never one of its answers), so Tab reaches them and no key answers by
 * itself. Answered with the keyboard inside it, a card gives the keyboard
 * back to the message box, or to the transcript in an agent's chat.
 */
function cardKeyboard(dock: () => HTMLElement | undefined) {
  let holding = false;
  onMount(() => {
    const element = dock();
    if (!element) return;
    const active = document.activeElement;
    const idle = !active || active === document.body;
    const agentChat =
      !document.querySelector("[data-composer-input]") &&
      !!active?.closest(".conversation") &&
      !isTextEntry(active);
    if (idle || agentChat) element.focus({ preventScroll: true });
  });
  onCleanup(() => {
    if (!holding) return;
    queueMicrotask(() => {
      const active = document.activeElement;
      // The next card took it, or the person put it somewhere.
      if (active && active !== document.body && active.isConnected) return;
      const box = document.querySelector<HTMLTextAreaElement>("[data-composer-input]");
      (box && !box.disabled ? box : document.querySelector<HTMLElement>(".transcript"))?.focus({
        preventScroll: true,
      });
    });
  });
  return (element: HTMLElement) => {
    element.addEventListener("focusin", () => (holding = true));
    element.addEventListener("focusout", (event) => {
      // Focus lost with its button (answered, disabled) is still held here.
      const next = event.relatedTarget;
      if (next instanceof Node && !element.contains(next)) holding = false;
    });
  };
}
/** Enter that confirms an input method's composition belongs to the input method. */
const composing = (event: KeyboardEvent) => event.isComposing || event.keyCode === 229;

function Question(props: { question: T.PendingQuestionInfo }) {
  const app = useApp();
  const [answer, setAnswer] = createSignal(""),
    [choices, setChoices] = createSignal<string[]>([]),
    [answered, setAnswered] = createSignal(false);
  let dock!: HTMLElement, input: HTMLInputElement | undefined;
  // A question that takes several answers sends the marked choices and then
  // what was typed; one that takes a single answer sends the one shown,
  // typed or chosen (typing unmarks a choice, and choosing clears the text).
  const reply = () =>
    props.question.multiple
      ? [...choices(), answer().trim()].filter(Boolean).join(", ")
      : answer().trim() || choices()[0] || "";
  const submit = () => {
    const value = reply();
    if (!value || answered()) return;
    setAnswered(true);
    void app.run(async () => {
      try {
        await app.api("replyQuestion", props.question.questionId, {
          answer: value,
        });
        await app.refresh();
      } catch (error) {
        setAnswered(false);
        throw error;
      }
    });
  };
  const holdKeyboard = cardKeyboard(() => dock);
  // One answer, and nothing to write instead: choosing it is the reply, as
  // Enter on it is (and as in the TUI).
  const answersOnClick = () => !props.question.multiple && props.question.custom === false;
  return (
    <section
      ref={(element) => {
        dock = element;
        holdKeyboard(element);
      }}
      class="prompt-dock question"
      tabindex="-1"
      aria-label={t("A question for you")}
      onKeyDown={(event) => {
        // Cmd/Ctrl+Enter replies from anywhere in the card, with what is
        // typed or chosen; a plain Enter keeps its own meaning there.
        if (
          event.defaultPrevented ||
          event.key !== "Enter" ||
          !(event.metaKey || event.ctrlKey) ||
          event.altKey ||
          event.shiftKey ||
          composing(event)
        )
          return;
        event.preventDefault();
        if (!event.repeat) submit();
      }}
    >
      <header class="prompt-head">
        <Icon name="chat" size="sm" />
        <strong class="prompt-title">{t("A question for you")}</strong>
      </header>
      <p class="question-prompt">{props.question.prompt}</p>
      <div class="question-options">
        <For each={props.question.choices ?? []}>
          {(choice) => (
            <button
              classList={{ active: choices().includes(choice) }}
              aria-pressed={choices().includes(choice)}
              onClick={() => {
                if (!props.question.multiple) {
                  setChoices([choice]);
                  setAnswer("");
                  if (answersOnClick()) submit();
                  return;
                }
                setChoices((old) =>
                  old.includes(choice) ? old.filter((c) => c !== choice) : [...old, choice],
                );
              }}
              onKeyDown={(event) => {
                // Enter on a choice of a single-answer question picks it and
                // replies, as in the TUI. On a question that takes several,
                // Enter only marks or unmarks it, as a click does.
                if (
                  props.question.multiple ||
                  event.key !== "Enter" ||
                  event.metaKey ||
                  event.ctrlKey ||
                  event.altKey ||
                  event.shiftKey ||
                  composing(event)
                )
                  return;
                event.preventDefault();
                if (event.repeat) return;
                setChoices([choice]);
                setAnswer("");
                submit();
              }}
            >
              {choice}
            </button>
          )}
        </For>
      </div>
      <footer class="prompt-actions">
        <Show when={props.question.custom !== false}>
          <input
            ref={input}
            aria-label={t("Answer")}
            placeholder={props.question.multiple ? t("Add another answer…") : t("Or write your answer…")}
            value={answer()}
            onInput={(event) => {
              setAnswer(event.currentTarget.value);
              if (!props.question.multiple && answer().trim()) setChoices([]);
            }}
            onKeyDown={(event) => {
              if (
                event.key !== "Enter" ||
                event.shiftKey ||
                composing(event) ||
                !answer().trim()
              )
                return;
              event.preventDefault();
              if (!event.repeat) submit();
            }}
          />
        </Show>
        <button
          class="primary"
          disabled={answered() || !reply()}
          onClick={submit}
        >
          {t("Reply")}
        </button>
      </footer>
    </section>
  );
}
