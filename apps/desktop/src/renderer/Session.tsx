import { measureRow } from "./measure-row";
import { t, splitAround, parseLanguage } from "./i18n";
import { InputQueue } from "./InputQueue";
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
import { PromptContextControls } from "./PromptContext";
import { mentionAgents } from "./prompt-context";
import { sessionKey } from "../projects";
import { agentColor } from "./agent-colors";
import { conversationAgents, selectConversationAgent } from "./agent-selection";
import { providerModelOptions } from "./provider-model-options";
import { canAutoApprove } from "./approval-mode";
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
} from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import type * as T from "@haarness/sdk";
import { useApp } from "./state";
import { Button, Icon, ask, stringify, time, type IconName } from "./ui";
import { attachmentName, type Attachment } from "./media";
import { createFileDrop } from "./file-drop";
import { Markdown } from "./Markdown";
import { MenuButton, type MenuAction } from "./MenuButton";
import { isWorktree, locationName } from "../projects";
import { createWorktree } from "./worktrees";
import { ToolActivity } from "./ToolActivity";
import { Mark } from "./brand";
import { matchesShortcut } from "./search";
import {
  describeTool,
  errorRecovery,
  permissionCommand,
  shortPath,
  toolActivity,
  toolName,
  toolResult,
} from "./transcript";

/* The plan already has a home above the composer, so its bookkeeping calls do
   not also belong in the transcript. */
const PLAN_TOOLS = ["todo", "todowrite", "todoread"];

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
  const hasForkOrigin = () =>
    !!thread() &&
    !thread()!.isSubagent &&
    !!(thread()!.parentId || thread()!.forkOrigin);
  const forkSource = () =>
    state.threads.find((t) => t.id === thread()?.parentId);
  const forkSourceTitle = () =>
    forkSource()?.title || thread()?.forkOrigin?.title || "chat";
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
  const setAttachments = (value: Attachment[] | ((items: Attachment[]) => Attachment[])) =>
    app.setAttachmentDraft(app.draftKey(), value);
  const fileDrop = createFileDrop(
    (files) => { void app.run(() => app.attachFiles(app.draftKey(), files)); },
    () => !!project() && editable(),
  );
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
     wraps, so pressing again walks the whole set and returns. */
  function cycleAgent(step: 1 | -1) {
    const available = conversationAgents(state.agents);
    if (available.length < 2) return;
    const index = available.findIndex((agent) => agent.id === selectedAgent());
    const next = available[(index + step + available.length) % available.length];
    if (next && next.id !== selectedAgent()) chooseAgent(next.id);
  }
  /* The agent keys work from anywhere in the chat, not only the composer, as
     in the TUI. They yield to whatever claimed the key first — the mention and
     slash popovers, menus, the editor and the terminal all prevent default —
     and stay out of dialogs, where Tab has to keep moving focus. */
  onMount(() => {
    const keys = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || event.repeat) return;
      if (document.querySelector("dialog[open]")) return;
      const target = event.target instanceof Element ? event.target : null;
      if (
        target?.closest(
          ".cm-editor, .xterm, [role='menu'], [role='listbox'], .select-popover, .action-menu",
        )
      )
        return;
      if (controls.busy()) return;
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
          cycleAgent(step);
          return;
        }
      }
    };
    document.addEventListener("keydown", keys);
    onCleanup(() => document.removeEventListener("keydown", keys));
  });
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
    if (!state.ready || !state.connection) return;
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
            description: "Manage project memory",
            template: "",
          },
          { name: "language", description: t("select_language"), template: "" },
          { name: "settings", description: t("Open settings"), template: "" },
          { name: "input-mode", description: "Toggle project Steer/Queue · /input-mode [steer|queue|toggle|reset]", template: "" },
          ...historyCommands,
          ...state.commands.filter(
            (item) =>
              item.name !== "language" && item.name !== "memory" && item.name !== "settings" && item.name !== "input-mode" &&
              !historyCommands.some((command) => command.name === item.name),
          ),
        ]
          .filter((item) =>
            item.name.toLowerCase().startsWith(draft().slice(1).toLowerCase()),
          )
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
  function pauseFollowing() {
    revealTarget = undefined;
    cancelAnimationFrame(revealFrame);
    if (scrolling.scrollHeight <= scrolling.clientHeight + 4) return;
    cancelAnimationFrame(scrollFrame);
    scrollFrame = 0;
    setFollow(false);
  }
  function followLatest() {
    if (!follow() || scrollFrame) return;
    scrollFrame = requestAnimationFrame(() => {
      scrollFrame = 0;
      // User input can arrive after a stream delta scheduled this frame.
      if (!follow() || !scrolling) return;
      scrolling.scrollTop = scrolling.scrollHeight;
      lastTop = scrolling.scrollTop;
    });
  }
  function onScroll() {
    const top = scrolling.scrollTop;
    const remaining = scrolling.scrollHeight - top - scrolling.clientHeight;
    // Height contraction and virtual-row measurement can reduce scrollTop
    // without user input. Upward input pauses synchronously in the handlers
    // below; geometry changes alone must not change the user's follow choice.
    if (!follow() && top > lastTop && remaining <= 4) setFollow(true);
    lastTop = top;
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
    onCleanup(() => {
      children.disconnect();
      resize.disconnect();
      cancelAnimationFrame(scrollFrame);
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
  const showStop = () =>
    !!active() &&
    !draft().trim() &&
    !attachments().length &&
    !app.promptContext().length;
  const [stopping, setStopping] = createSignal(false);
  const stop = async () => {
    setStopping(true);
    try {
      await app.api("interruptThreadTree", state.selected);
      await app.refresh();
    } finally {
      setStopping(false);
    }
  };
  const stream = () => (active() ? state.streams[active()!.id] : undefined);
  const [findOpen, setFindOpen] = createSignal(false);
  const [findMatch, setFindMatch] = createSignal("");
  type RestoredDraft = {
    owner: string;
    text: string;
    attachments: string;
    baseText: string;
    baseAttachments: { name: string; data: string }[];
  };
  let restoredDraft: RestoredDraft | undefined;
  const pristineUndoDraft = () =>
    restoredDraft?.owner === owner() &&
    restoredDraft.text === draft() &&
    restoredDraft.attachments === JSON.stringify(attachments());
  const closeFind = () => {
    setFindOpen(false);
    setFindMatch("");
    scrolling?.focus({ preventScroll: true });
  };
  const openFind = () => {
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
      const text = item.data.text;
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
  app.registerSearch("composer", () => {
    if (!project() || !editable() || state.openingProject) return [];
    return [
      {
        key: "focus-composer",
        group: "Conversation actions",
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
              group: "Conversation actions",
              name: voice.active() ? "Stop dictation" : "Start dictation",
              keywords: "microphone voice speech transcribe",
              icon: "mic" as const,
              action: async () => {
                set("panel", "chat");
                await voice.toggle();
              },
            },
          ]
        : []),
      ...(active()
        ? [
            {
              key: "stop-turn",
              group: "Conversation actions",
              name: t("Stop response"),
              keywords: "interrupt cancel turn",
              icon: "stop" as const,
              disabled: stopping(),
              action: stop,
            },
          ]
        : []),
      ...state.commands.map((command) => ({
        key: "slash:" + command.name,
        group: "Slash commands",
        name: "/" + command.name,
        detail: command.description || "Insert into message",
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
        !(item.data.type === "reasoning" && !state.preferences.showReasoning) &&
        item.data.type !== "tool_result" &&
        !(
          item.data.type === "tool_call" &&
          PLAN_TOOLS.includes(
            toolName((item.data as { name: string }).name ?? ""),
          )
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
  onMount(() => {
    scrolling.addEventListener("markdown-ready", settleReveal);
    onCleanup(() => {
      scrolling.removeEventListener("markdown-ready", settleReveal);
      cancelAnimationFrame(revealFrame);
    });
  });
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
    }
  }
  const ForkDivider = () => (
    <div class="fork-origin" role="note" aria-label={t("Conversation origin")}>
      <button
        class="fork-origin-link"
        disabled={!forkSource()}
        title={
          forkSource()
            ? `Open ${forkSourceTitle()}`
            : `Source conversation unavailable · ${forkSourceTitle()}`
        }
        onClick={() => app.run(openForkSource)}
      >
        <Icon name="branch" size="sm" />
        <span>Continued from {forkSourceTitle()}</span>
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
        setDraft(state.drafts[draftKey()] ?? "");
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
  function change(text: string, spoken = false) {
    resetRecall();
    if (!spoken) voice.cancel();
    setDraft(text);
    if (project()) {
      set("drafts", draftKey(), text);
      clearTimeout(draftTimer);
      const key = "draft:" + draftScope() + ":" + state.selected;
      draftSave = { key, text };
      draftTimer = setTimeout(persistDraft, 500);
    }
  }
  async function send(delivery?: T.InputDelivery) {
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
        if (connection === state.connection?.id) app.notify(result.data.message);
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
    if (
      !project() ||
      !state.ready ||
      !editable() ||
      submitting() ||
      controls.busy() ||
      stopping() ||
      (!showStop() &&
        !draft().trim() &&
        !attachments().length &&
        !app.promptContext().length)
    )
      return;
    if (!selectedModel()) {
      app.openConnect({ onboarding: true });
      return;
    }
    voice.cancel();
    let connection = project()?.id;
    let createdOwner = "";
    const options = {
      ...choice(),
      ...selectedModel(),
      agent: selectedAgent(),
    };
    const selected = state.selected;
    const origin = owner();
    const originProject = draftProject();
    const text = draft();
    const contextKey = app.draftKey();
    setSubmissions((items) => [...items, origin]);
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
        setDraft(text);
      }
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
        },
        context,
        delivery,
      );
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
      if (
        state.connection?.id === connection &&
        (state.selected === selected ||
          (!selected && state.selected === threadId))
      ) {
        if (draft() === text) {
          setDraft("");
          composer.focus();
        }
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
    if (tool) return toolActivity(tool.name, tool.arguments);
    return stream()?.text ? "Writing a reply" : "Thinking";
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
      ref={fileDrop.ref}
      class="conversation"
      classList={{ "empty-start": empty(), "file-drop-active": fileDrop.dragging() }}
      aria-label={t("Conversation")}
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
          {t("Updating conversation history…")}
        </div>
      </Show>
      <div
        class="transcript"
        ref={scrolling}
        tabIndex={0}
        aria-label={t("Conversation history")}
        onScroll={onScroll}
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
        }}
        onPointerDown={(event) => {
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
                fallback={splitAround(t("{project} to get started"), () => (
                  <ProjectPicker heading />
                ))}
              >
                {splitAround(t("What should we work on in {project}?"), () => (
                  <ProjectPicker heading />
                ))}
              </Show>
            </h2>
            <Show when={project()}>
              <div class="suggestions" aria-label={t("Conversation starters")}>
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
                  fork={() => app.run(() => fork(rows()[row.index]))}
                />
              </div>
            )}
          </For>
        </div>
        <Show when={hasForkOrigin() && forkDividerIndex() === -1}>
          <ForkDivider />
        </Show>
        <Show when={stream()?.reasoning && state.preferences.showReasoning}>
          <div class="message reasoning">
            <div class="message-label">
              <span class="author">{t("Thinking")}</span>
            </div>
            <Markdown text={stream()!.reasoning} live />
          </div>
        </Show>
        <Show when={stream()?.text}>
          <div class="message assistant" data-testid="streaming-reply">
            <Markdown text={stream()!.text} live />
          </div>
        </Show>
        <Show when={active() && !pending()}>
          <div class="assistant-activity" role="status" aria-live="polite">
            <Shimmer>{activity()}</Shimmer>
          </div>
        </Show>
        <div class="transcript-tail" />
      </div>
      <Show when={editable() || pending() || !follow() || todos().length}>
        <div class="composer-area">
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
                ? "Waiting for your response"
                : "Haarness is working"
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
            <InputQueue working={!!active()} />
            <div
              class="composer"
              data-agent={selectedAgent()}
              style={{
                "--composer-agent": agentColor(selectedAgent()),
              }}
              classList={{ running: !!active(), waiting: pending() }}
              onPaste={(event) => {
                const files = Array.from(event.clipboardData?.files ?? []);
                if (!files.length) return;
                event.preventDefault();
                void app.run(() => app.attachFiles(app.draftKey(), files));
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
                          onClick={() =>
                            setAttachments((items) =>
                              items.filter((_, i) => i !== index()),
                            )
                          }
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
                disabled={
                  submitting() ||
                  controls.busy() ||
                  !project() ||
                  !state.ready ||
                  !!state.openingProject
                }
                placeholder={
                  !project()
                    ? t("Choose a project to start a conversation")
                    : active()
                      ? t("Send a follow-up message…")
                      : state.selected
                        ? t("Reply, or @ to add files and agents")
                        : t("Do anything, or @ to add files and agents")
                }
                value={draft()}
                onInput={(event) => change(event.currentTarget.value)}
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
                disabled={submitting() || stopping() || !state.ready}
              />
              <Show when={slashOpen()}>
                <div
                  id="slash-commands"
                  class="slash-suggestions popover"
                  role="listbox"
                  aria-label={t("Slash commands")}
                >
                  <div class="slash-suggestions-header">
                    {t("Commands")}<span>↑ ↓ to browse · Enter to select</span>
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
                            ? " — " + command.description
                            : "")
                        }
                        onPointerMove={() => setSlashIndex(index())}
                        onMouseDown={(event) => event.preventDefault()}
                        onClick={() => chooseCommand(index())}
                      >
                        <strong>/{command.name}</strong>
                        <span>{command.description}</span>
                      </button>
                    )}
                  </For>
                </div>
              </Show>
              <footer>
                <div class="composer-leading">
                  <Button
                    icon="plus"
                    label={t("Attach files")}
                    disabled={!project()}
                    onClick={() =>
                      app.run(async () => {
                        const key = app.draftKey();
                        const values = await window.haarness.attachments();
                        await app.addAttachments(key, values);
                      })
                    }
                  />
                  <Show when={state.connection}>
                    <button
                      class="pill-select approval-mode"
                      classList={{ enabled: app.isAutoApproval() }}
                      disabled={!!state.openingProject}
                      aria-label={t("Automatic approvals")}
                      aria-pressed={app.isAutoApproval()}
                      title={t(
                        app.isAutoApproval()
                          ? state.selected
                            ? "Auto allows requests once for this conversation and its agents. Click to switch."
                            : "Auto allows requests once for this workspace. Click to switch."
                          : state.selected
                            ? "Review permission requests for this conversation and its agents. Click to switch."
                            : "Review permission requests for this workspace. Click to switch.",
                      )}
                      onClick={() =>
                        app.run(() =>
                          app.setAutoApproval(!app.isAutoApproval()),
                        )
                      }
                    >
                      <Icon name="shield" size="sm" />
                      {app.isAutoApproval() ? t("Auto-approve") : t("Ask first")}
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
                    <Show
                      when={selectedModel()}
                      fallback={
                        <button
                          class="pill-select composer-model"
                          disabled={!state.ready}
                          onClick={() => app.openConnect({ onboarding: true })}
                        >
                          {state.ready
                            ? "Connect a provider"
                            : "Loading models…"}
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
                        voice.active() ? "Stop dictation" : "Start dictation"
                      }
                      title={
                        voice.active()
                          ? "Stop dictation"
                          : "Start dictation (⌘⇧M)"
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
                    aria-label={
                      showStop()
                        ? "Stop session"
                        : active()
                          ? "Send message"
                          : "Send message"
                    }
                    disabled={
                      !project() ||
                      submitting() ||
                      controls.busy() ||
                      stopping() ||
                      (!showStop() && !state.ready) ||
                      (!showStop() &&
                        !draft().trim() &&
                        !attachments().length &&
                        !app.promptContext().length)
                    }
                    onClick={() => app.run(showStop() ? stop : () => send())}
                  >
                    <Icon
                      name={showStop() ? "stop" : "send"}
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
function Message(props: {
  item: T.Item;
  result?: T.Item;
  task?: Task;
  fork: () => Promise<unknown>;
}) {
  const app = useApp();
  const content = () => props.item?.data;
  const kind = () => {
    const type = content().type;
    return type === "user_message"
      ? "user"
      : type === "reasoning"
        ? "reasoning"
        : props.task || type === "tool_call" || type === "compaction"
          ? "step"
          : "assistant";
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
        aria-label={copied() ? "Copied" : "Copy message"}
        title={copied() ? "Copied" : "Copy message"}
        onClick={() =>
          app.run(() => copy((content() as { text: string }).text))
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
              <Markdown text={(content() as { text: string }).text} />
              <For
                each={
                  (content() as Extract<T.ItemData, { type: "user_message" }>)
                    .attachments
                }
              >
                {(attachment) => (
                  <span class="attachment-chip">
                    {attachment.startsWith("data:image/") ? (
                      <img src={attachment} alt={t("Attached image")} />
                    ) : (
                      <>
                        <Icon name="file" size="sm" />
                        <span class="attachment-name">
                          {attachmentName(attachment)}
                        </span>
                      </>
                    )}
                  </span>
                )}
              </For>
            </div>
            <div class="message-footer">
              <span class="message-time">{time(props.item.createdAt)}</span>
              <MessageTools />
            </div>
          </Match>

          <Match when={content().type === "assistant_message"}>
            <Markdown text={(content() as { text: string }).text} />
            <div class="message-footer">
              <span class="message-time">{time(props.item.createdAt)}</span>
              <MessageTools />
            </div>
          </Match>

          <Match when={content().type === "reasoning"}>
            <div class="message-label">
              <span class="author">{t("Thinking")}</span>
              <span class="message-time">{time(props.item.createdAt)}</span>
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
                  expanded={!app.state.preferences.compactTools}
                />
              );
            })()}
          </Match>

          <Match when={content().type === "error"}>
            {(() => {
              const message = () => (content() as { message: string }).message;
              const recovery = () => errorRecovery(message());
              return (
                <div class="inline-error" role="alert">
                  <Icon name="error" size="sm" />
                  <span>{message()}</span>
                  <Show when={recovery()}>
                    <button
                      class="inline-error-action"
                      onClick={() => app.set("settings", recovery()!.pane)}
                    >
                      {recovery()!.label}
                    </button>
                  </Show>
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
          "Always allow applies only to this agent in this session.",
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
  const decide = (decision: string) => {
    if (answered()) return;
    const connection = app.state.connection!.id;
    const id = props.permission.permissionId;
    setAnswered(true);
    void app.run(async () => {
      try {
        if (decision === "auto") await app.allowOnceAndAuto(props.permission);
        else
          await app.api("replyPermission", props.permission.permissionId, {
            decision,
          });
        await app.refresh();
      } catch (error) {
        setAnswered(false);
        app.recordPermissionError(connection, id, error);
        throw error;
      }
    });
  };
  const title = () =>
    permissionTitles[
      props.permission.permission || props.permission.operation.split(":")[0]
    ] ?? "Permission needed";
  const alwaysChoices = (): MenuAction[] => [
    {
      label: t("Always for this folder"),
      detail: props.permission.alwaysPatterns?.join(", ") || shortPath(app.state.connection?.workspace ?? ""),
      icon: "folder",
      run: () => decide("always_workspace"),
    },
    {
      label: t("Always for this repository"),
      detail: t("Every linked worktree of {repository}", {
        repository: shortPath(props.permission.repositoryScope ?? ""),
      }),
      icon: "branch",
      run: () => decide("always_repository"),
    },
  ];
  return (
    <section
      class="prompt-dock approval"
      role="alert"
      aria-label={t("Permission request")}
    >
      <header class="prompt-head">
        <Icon name={shape().icon} size="sm" />
        <strong class="prompt-title">{t(title())}</strong>
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
            {t("View agent session")}
          </button>
        </Show>
      </header>
      <Show
        when={permissionCommand(props.permission.detail)}
        fallback={<pre class="prompt-subject prompt-detail">{detail()}</pre>}
      >
        <pre class="prompt-subject approval-command">
          {permissionCommand(props.permission.detail)}
        </pre>
      </Show>
      <Show when={app.permissionError(props.permission.permissionId)}>
        <p class="error" role="alert" data-testid="permission-reply-error">
          {app.permissionError(props.permission.permissionId)}
        </p>
      </Show>
      <footer class="prompt-actions">
        <button
          class="ghost"
          disabled={answered()}
          onClick={() => decide("reject")}
        >
          {t("Deny")}
        </button>
        <Show when={props.permission.allowAlways !== false}>
          <Show
            when={props.permission.repositoryScope}
            fallback={
              <button
                disabled={answered()}
                onClick={() => decide("always_workspace")}
                title={props.permission.alwaysPatterns?.join("\n")}
              >
                {t("Always for this folder")}
              </button>
            }
          >
            <MenuButton
              label={t("Always allow")}
              content={
                <>
                  {t("Always allow")}
                  <Icon name="down" size="xs" />
                </>
              }
              class="prompt-always"
              title={props.permission.alwaysPatterns?.join("\n") || t("Always allow")}
              items={alwaysChoices()}
            />
          </Show>
        </Show>
        <Show when={!app.isAutoApproval() && canAutoApprove(props.permission)}>
          <button
            disabled={answered()}
            onClick={() => decide("auto")}
            title={t("Allow this request once and enable Auto for this conversation and its agents")}
          >
            {t("Allow once + Auto")}
          </button>
        </Show>
        <button
          class="primary"
          disabled={answered()}
          onClick={() => decide("once")}
        >
          {t("Allow once")}
        </button>
      </footer>
    </section>
  );
}
function Question(props: { question: T.PendingQuestionInfo }) {
  const app = useApp();
  const [answer, setAnswer] = createSignal(""),
    [choices, setChoices] = createSignal<string[]>([]);
  return (
    <section class="prompt-dock question" aria-label={t("A question for you")}>
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
              onClick={() => {
                setChoices((old) =>
                  props.question.multiple
                    ? old.includes(choice)
                      ? old.filter((c) => c !== choice)
                      : [...old, choice]
                    : [choice],
                );
                setAnswer("");
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
            aria-label={t("Answer")}
            placeholder={t("Or write your answer…")}
            value={answer()}
            onInput={(event) => setAnswer(event.currentTarget.value)}
          />
        </Show>
        <button
          class="primary"
          disabled={!answer() && !choices().length}
          onClick={() =>
            app.run(async () => {
              await app.api("replyQuestion", props.question.questionId, {
                answer: answer() || choices().join(", "),
              });
              await app.refresh();
            })
          }
        >
          {t("Reply")}
        </button>
      </footer>
    </section>
  );
}
