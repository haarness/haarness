import { t } from "./i18n";
import { ProviderLabel } from "./ProviderIcon";
import { modelName } from "../model-names";
import { toolActivity } from "./transcript";
import { Shimmer } from "./Shimmer";
import { Show, createMemo, createSignal } from "solid-js";
import type { Turn } from "@haarness/sdk";
import { useApp } from "./state";
import { Icon } from "./ui";
import {
  agentTaskActiveTurn,
  agentTaskError,
  agentTaskStatus,
  type AgentTask as Task,
} from "./agent-tasks";

export function AgentTask(props: { task: Task }) {
  const app = useApp();
  let openButton!: HTMLButtonElement;
  const [stopping, setStopping] = createSignal(false);
  const childId = () => props.task.link?.child_thread_id;
  const child = () =>
    app.state.threads.find((thread) => thread.id === childId());
  const progress = () => app.state.tasks?.[props.task.key];
  const args = () => {
    const value = props.task.call?.arguments;
    return value && typeof value === "object" && !Array.isArray(value)
      ? value
      : {};
  };
  const text = (value: unknown) =>
    typeof value === "string" ? value.trim() : "";
  const error = () => agentTaskError(props.task);
  const title = () =>
    text(args().description) ||
    child()?.title ||
    text(args().prompt) ||
    t("Agent task");
  const agent = () => {
    const id =
      child()?.agent || text(args().agent) || text(args().subagent_type);
    return (
      app.state.agents.find((agent) => agent.id === id)?.name || id || t("Agent")
    );
  };
  const turn = () =>
    app.state.turns.find((turn) => turn.threadId === childId());
  const model = () => {
    const id = child()?.model || turn()?.model;
    const provider = child()?.provider || turn()?.provider;
    return modelName(provider, id, app.state.providers);
  };
  const status = createMemo(() => {
    const base = agentTaskStatus(props.task, progress());
    if (["completed", "failed", "stopped"].includes(base)) return base;
    if (app.state.permissions.some((item) => item.threadId === childId()))
      return "approval";
    if (app.state.questions.some((item) => item.threadId === childId()))
      return "question";
    return base;
  });
  const label = () =>
    ({
      completed: t("Completed"),
      stopped: t("Stopped"),
      failed: t("Failed"),
      approval: t("Needs approval"),
      question: t("Needs an answer"),
      starting: t("Starting…"),
      background: t("Working in background"),
      running: t("Working"),
    })[status()];
  const working = () =>
    ["starting", "running", "background"].includes(status());
  const active = createMemo(() =>
    agentTaskActiveTurn(props.task, progress(), app.state.turns),
  );
  const stop = async () => {
    const target = active();
    if (!target || stopping()) return;
    // Keep focus in the row when the stop control disappears on completion.
    openButton.focus({ preventScroll: true });
    setStopping(true);
    try {
      const current = await app.api<Turn | null>(
        "activeTurn",
        target.threadId,
      );
      if (
        current?.id === target.id &&
        ["running", "queued"].includes(current.status)
      )
        await app.api("interruptThreadTree", target.threadId);
      await app.refresh();
    } finally {
      setStopping(false);
    }
  };
  const detail = () => {
    if (!working()) return "";
    const name = progress()?.lastTool;
    return name ? toolActivity(name) : "";
  };
  return (
    <div class="agent-task-row">
      <button
        ref={openButton}
        type="button"
        class="agent-task"
        data-status={status()}
        data-child-id={childId()}
        disabled={!childId()}
        aria-label={`${childId() ? t("Open agent task") : t("Agent task")}: ${title()}`}
        aria-description={label()}
        onClick={() => app.run(() => app.choose(childId()!))}
      >
        <span class="agent-task-mark" title={label()} aria-hidden="true">
          <Icon
            name={
              status() === "approval"
                ? "alert"
                : status() === "question"
                  ? "question"
                  : "agents"
            }
            size="md"
          />
        </span>
        <span class="agent-task-body">
          <span class="agent-task-title" title={title()}>
            <Shimmer active={working()}>{title()}</Shimmer>
          </span>
          <Show when={props.task.result?.is_error}>
            <span class="agent-task-error" title={error()}>
              {error()}
            </span>
          </Show>
          <span class="agent-task-meta">
            <span class="agent-task-agent">{agent()}</span>
            <Show when={model()}>
              <span class="agent-task-model">
                <ProviderLabel
                  provider={child()?.provider || turn()?.provider}
                  size="xs"
                >
                  {model()}
                </ProviderLabel>
              </span>
            </Show>
            <Show when={detail()}>
              <span class="agent-task-detail">{detail()}</span>
            </Show>
          </span>
        </span>
        <span class="agent-task-status" title={label()} aria-hidden="true">
          <Show when={status() === "completed"}>
            <Icon name="check" size="xs" />
          </Show>
          <Show when={status() === "failed"}>
            <Icon name="error" size="xs" />
          </Show>
          <Show when={status() === "stopped"}>
            <Icon name="stop" size="xs" />
          </Show>
        </span>
        <Icon name="chevron" size="xs" class="agent-task-chevron" />
      </button>
      <Show when={active()}>
        <button
          type="button"
          class="icon-button agent-task-stop"
          aria-label={`${stopping() ? t("Stopping agent task") : t("Stop agent task")}: ${title()}`}
          title={stopping() ? t("Stopping…") : t("Stop agent")}
          disabled={stopping()}
          onClick={() => void app.run(stop)}
        >
          <Icon name="stop" size="xs" />
        </button>
      </Show>
    </div>
  );
}
