import { PreviewBanner } from "./PreviewBanner";
import { previewInfo } from "./preview-actions";
import { t } from "./i18n";
import {
  For,
  Index,
  Show,
  createEffect,
  createMemo,
  createSignal,
} from "solid-js";
import { Icon } from "./ui";
import { Shimmer } from "./Shimmer";
import { lines } from "./diff";
import {
  batchLabel,
  batchSteps,
  describeTool,
  toolChanges,
  toolInput,
  toolName,
  toolOutput,
  toolFailed,
  routineEditOutput,
  CHANGE_PREVIEW_LINES,
  type Change,
  type ToolResult,
} from "./transcript";

/** A quiet action row; its disclosure shows the action and readable results. */
export function ToolActivity(props: {
  name: string;
  args: unknown;
  result?: ToolResult;
  expanded?: boolean;
  nested?: boolean;
  settled?: boolean;
  /** Its turn is waiting for the person to approve a request. */
  waiting?: boolean;
}) {
  const [open, setOpen] = createSignal(!!props.expanded);
  createEffect(() => setOpen(!!props.expanded));
  const batch = createMemo(() =>
    !props.nested && toolName(props.name) === "batch"
      ? batchSteps(props.args, props.result)
      : [],
  );
  // A grouped row names what happened, not how many calls it took: a count
  // invites the reader to wonder about it, and the disclosure lists them.
  const shape = createMemo(() =>
    batch().length
      ? {
          icon: "layers" as const,
          verb: batchLabel(batch()),
          doing: batchLabel(batch()),
          attempt: batchLabel(batch()),
          target: "",
        }
      : describeTool(props.name, props.args),
  );
  const status = () =>
    props.result
      ? toolFailed(props.name, props.result) ||
        batch().some((step) => toolFailed(step.name, step.result))
        ? "failed"
        : "done"
      : props.nested
        ? "pending"
        : "running";
  // Waiting on the person is not work in motion: the row rests and says so.
  const waiting = () => !!props.waiting && !props.result && !props.settled;
  const live = () =>
    !waiting() &&
    (status() === "running" || (status() === "pending" && !props.settled));
  // The verb tells the row's state: "Writing" while it runs or waits for
  // approval, "Wrote" once done, "Tried to write" when the tool errored or
  // was refused. A command that ran and exited non-zero still "Ran".
  const verb = () =>
    props.result
      ? props.result.is_error
        ? shape().attempt
        : shape().verb
      : props.settled
        ? shape().attempt
        : shape().doing;
  const changes = createMemo(() =>
    toolChanges(toolName(props.name), props.args, props.result),
  );
  // An edit's size is the first thing a reader wants after its file name.
  const stats = createMemo(() => {
    let additions = 0,
      deletions = 0;
    for (const change of changes()) {
      if (change.additions !== undefined && change.deletions !== undefined) {
        additions += change.additions;
        deletions += change.deletions;
        continue;
      }
      for (const row of changeRows(change))
        if (row.kind === "add") additions++;
        else if (row.kind === "remove") deletions++;
    }
    return additions || deletions ? { additions, deletions } : undefined;
  });
  const input = createMemo(() => toolInput(props.name, props.args));
  const output = createMemo(
    () => props.result && toolOutput(props.name, props.result.output),
  );
  return (
    <Show when={toolName(props.name) === "preview" && props.result && !props.result.is_error && previewInfo(props.result.metadata)} fallback={<details
      class="tool-call"
      classList={{
        "tool-child": props.nested,
        "tool-batch": !!batch().length,
      }}
      data-status={status()}
      open={open()}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>
        <span class="tool-mark" aria-hidden="true">
          <Icon name={shape().icon} size="sm" />
        </span>
        {/* A step in motion sweeps like the activity line and the agent card;
            a finished one simply rests. The word stays for screen readers. */}
        <Shimmer class="tool-label" active={live()}>
          <strong>{verb()}</strong>
          <span class="tool-target" title={shape().target}>
            {shape().target}
          </span>
          <Show when={stats()}>
            {(count) => (
              <span class="tool-stats">
                <Show when={count().additions}>
                  <span class="tool-stat-add">+{count().additions}</span>
                </Show>
                <Show when={count().deletions}>
                  <span class="tool-stat-remove">−{count().deletions}</span>
                </Show>
              </span>
            )}
          </Show>
        </Shimmer>
        <Show when={live()}>
          <span class="sr-only">
            {status() === "running" ? t("Running") : t("Waiting for result")}
          </span>
        </Show>
        <Show when={waiting()}>
          <span class="tool-status waiting">{t("Needs approval")}</span>
        </Show>
        <Show when={status() === "pending" && props.settled}>
          <span class="tool-status">{t("No result")}</span>
        </Show>
        <Icon name="chevron" size="xs" class="tool-chevron" />
      </summary>
      <Show when={open()}>
        <div class="tool-body">
          <Show
            when={batch().length}
            fallback={
              <>
                <Show
                  when={changes().length}
                  fallback={
                    <div class="tool-console">
                      <div class="tool-console-label">
                        {input().label}
                        <Show when={input().context}>
                          <span>{input().context}</span>
                        </Show>
                      </div>
                      <Show when={input().text}>
                        <pre class="tool-input">{input().text}</pre>
                      </Show>
                      <Show
                        when={props.result}
                        fallback={
                          <p class="tool-empty">
                            {props.settled
                              ? t("No result was recorded.")
                              : t("Waiting for output…")}
                          </p>
                        }
                      >
                        <Show
                          when={output()}
                          fallback={<p class="tool-empty">{t("No output")}</p>}
                        >
                          <pre class="tool-output">{output()}</pre>
                        </Show>
                      </Show>
                    </div>
                  }
                >
                  <For each={changes()}>
                    {(change, index) => (
                      <ChangePreview
                        change={change}
                        showPath={index() > 0 || changes().length > 1}
                      />
                    )}
                  </For>
                  <Show
                    when={
                      output() &&
                      (props.result?.is_error || !routineEditOutput(output()!))
                    }
                  >
                    <div class="tool-console">
                      <pre class="tool-output">{output()}</pre>
                    </div>
                  </Show>
                </Show>
              </>
            }
          >
            <div class="tool-children">
              <Index each={batch()}>
                {(step) => (
                  <ToolActivity
                    name={step().name}
                    args={step().args}
                    result={step().result}
                    nested
                    settled={!!props.result}
                  />
                )}
              </Index>
            </div>
            <Show
              when={
                props.result &&
                (props.result.is_error ||
                  batch().some((step) => !step.result))
              }
            >
              <div class="tool-console">
                <pre class="tool-output">{output()}</pre>
              </div>
            </Show>
          </Show>
        </div>
      </Show>
    </details>}><PreviewBanner metadata={props.result?.metadata} /></Show>
  );
}
function changeRows(change: Change) {
  return lines({
    file: change.path || "change",
    patch: change.patch,
    before: change.before,
    after: change.after,
  } as Parameters<typeof lines>[0]).filter((row) => row.kind !== "hunk");
}
export function ChangePreview(props: { change: Change; showPath?: boolean }) {
  const rows = createMemo(() => changeRows(props.change));
  const shown = () => rows().slice(0, CHANGE_PREVIEW_LINES);
  const hidden = () => rows().length - shown().length;
  return (
    <div class="change-preview">
      <Show when={props.showPath && props.change.path}>
        <div class="change-path">{props.change.path}</div>
      </Show>
      <div class="change-lines">
        <For each={shown()}>
          {(row) => (
            <div class={"diff-line " + row.kind}>
              <code>{row.text || " "}</code>
            </div>
          )}
        </For>
      </div>
      <Show when={hidden() > 0}>
        <p class="change-more">
          {t("{count} more lines · open Review to see all", { count: hidden() })}
        </p>
      </Show>
    </div>
  );
}
