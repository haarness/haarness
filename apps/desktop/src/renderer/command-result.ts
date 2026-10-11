import type { CommandExecutionResponse, WorkflowRun } from "@haarness/sdk";
import { t } from "./i18n";

/** A command's answer, shown above the composer of its chat until dismissed. */
export type CommandNote = { title: string; text: string };

/**
 * A prompt command answers in the transcript as a turn. Management commands
 * (/workflow, /goal status…) answer with text and workflow commands with their
 * run; the TUI shows both in its status line, so Desktop keeps them in view.
 */
export function commandNote(
  command: string,
  response: CommandExecutionResponse,
): CommandNote | null {
  const title = command.trim().split("\n", 1)[0];
  if (response.kind === "text")
    return response.text.trim() ? { title, text: response.text.trim() } : null;
  if (response.kind === "workflow_run")
    return { title, text: workflowRunText(response.run) };
  return null;
}

function workflowRunText(run: WorkflowRun) {
  const name = run.workflowName;
  const status = {
    running: t("Workflow {name} is running.", { name }),
    paused: t("Workflow {name} is paused.", { name }),
    completed: t("Workflow {name} finished.", { name }),
    failed: t("Workflow {name} failed.", { name }),
    cancelled: t("Workflow {name} was cancelled.", { name }),
    interrupted: t("Workflow {name} was interrupted.", { name }),
  }[run.status];
  // The run ID is what /workflow cancel, pause and resume take.
  return [status, run.error?.trim(), t("Run ID: {id}", { id: run.id })]
    .filter(Boolean)
    .join("\n");
}
