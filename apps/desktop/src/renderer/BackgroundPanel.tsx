import { For, Show } from "solid-js";
import { t } from "./i18n";
import { AgentTask } from "./AgentTask";
import { ToolActivity } from "./ToolActivity";
import { liveCommand } from "./shell-commands";
import type { backgroundWork } from "./background-work";
import "./background-panel.css";

/**
 * Still running: what the open chat left working in the background, its
 * subagents and then its agent's commands, each as its row reads in the
 * chat. A command stops from its row and opens to its live output; what
 * ended lately stays a while, greyed, so its end can still be read.
 */
export function BackgroundPanel(props: { work: ReturnType<typeof backgroundWork> }) {
  const ended = (state: string | undefined) => ["completed", "failed", "stopped"].includes(state ?? "");
  return (
    // The tab names the panel, so it has no heading of its own.
    <section class="background-work" aria-label={t("Still running")}>
      <Show
        when={props.work.subagents().length || props.work.commands().length}
        fallback={<p class="background-empty">{t("Nothing left running")}</p>}
      >
        <div class="background-list">
          <For each={props.work.subagents()}>
            {(key) => (
              <Show when={props.work.task(key)}>
                {(task) => (
                  <div class="background-item" classList={{ ended: ended(props.work.status(key)) }}>
                    <AgentTask task={task()} />
                  </div>
                )}
              </Show>
            )}
          </For>
          <For each={props.work.commands()}>
            {(command) => (
              <div class="background-item" classList={{ ended: !liveCommand(command.state) }}>
                <ToolActivity
                  name="bash"
                  args={{ command: command.command, ...(command.workdir ? { workdir: command.workdir } : {}) }}
                  command={command}
                />
              </div>
            )}
          </For>
        </div>
      </Show>
    </section>
  );
}
