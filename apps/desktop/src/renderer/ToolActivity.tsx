import { PreviewBanner } from "./PreviewBanner";
import { McpAppView } from "./McpAppView";
import { payloadFor as mcpAppPayload, type Payload as McpAppPayload } from "./mcp-app";
import { previewInfo } from "./preview-actions";
import { t } from "./i18n";
import {
  For,
  Index,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
} from "solid-js";
import {
  imageProject,
  onProjectImageSettled,
  projectFileWritten,
  projectRoots,
  readProjectImage,
  readToolImage,
  shownProjectImage,
  type ShownImage,
} from "./project-images";
import { watchOpenImages } from "./ImageViewer";
import { Icon } from "./ui";
import { requestReview } from "./Review";
import { Shimmer } from "./Shimmer";
import { SpelledOut, SpelledText } from "./SpelledOut";
import { turnsDirection } from "./display-text";
import { useApp } from "./state";
import { appsOff, loadMcpServers, mcpServersKnown, readOnlyServer, setApps } from "./mcp-servers";
import { markSelection, selectedByClick, type SelectionMark } from "./selection-click";
import {
  answeredByHook,
  batchLabel,
  batchSteps,
  batchTarget,
  describeTool,
  toolChanges,
  exitStatus,
  resultText,
  toolInput,
  toolName,
  toolTitle,
  imageName,
  toolFailed,
  toolVerb,
  ranInChrome,
  reviewBlock,
  routineEditOutput,
  changeRows,
  changeStats,
  CHANGE_PREVIEW_LINES,
  type Change,
  type ToolResult,
} from "./transcript";

/** A quiet action row; its disclosure shows the action and readable results. */
export function ToolActivity(props: {
  name: string;
  args: unknown;
  result?: ToolResult;
  /** The call's own item id, which identifies an MCP App page across remounts. */
  id?: string;
  expanded?: boolean;
  nested?: boolean;
  settled?: boolean;
  /** Its turn is waiting for the person to approve a request. */
  waiting?: boolean;
}) {
  const [open, setOpen] = createSignal(!!props.expanded);
  let pressed: SelectionMark | undefined;
  createEffect(() => setOpen(!!props.expanded));
  const batch = createMemo(() =>
    !props.nested && toolName(props.name) === "batch"
      ? batchSteps(props.args, props.result)
      : [],
  );
  // A grouped row names what happened, not how many calls it took: a count
  // invites the reader to wonder about it, and the disclosure lists them.
  // Its target names what the steps acted on, since rows start collapsed.
  const shape = createMemo(() =>
    batch().length
      ? {
          icon: "layers" as const,
          verb: batchLabel(batch()),
          doing: batchLabel(batch()),
          attempt: batchLabel(batch()),
          target: batchTarget(batch()),
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
  // was refused. A command that ran and exited non-zero still "Ran". The
  // forms are English keys; a batch's label arrives already translated.
  // Auto's reviewer refused the call: the row says so, and its body gives
  // the reason rather than the text written for the model.
  const blocked = createMemo(() => reviewBlock(props.result));
  const verb = () => {
    if (blocked() !== undefined) return t("Stopped by Auto's reviewer");
    const form = props.result
      ? props.result.is_error
        ? shape().attempt
        : shape().verb
      : props.settled
        ? shape().attempt
        : shape().doing;
    return batch().length ? form : toolVerb(form);
  };
  const changes = createMemo(() =>
    toolChanges(toolName(props.name), props.args, props.result),
  );
  // An edit's size is the first thing a reader wants after its file name.
  const stats = createMemo(() => changeStats(changes()));
  const input = createMemo(() => toolInput(props.name, props.args));
  const output = createMemo(
    () => props.result && (blocked() || resultText(props.name, props.result)),
  );
  const exit = () => exitStatus(props.name, props.result);
  // A question's answer, after it: the row then says what was asked and
  // what came back, in the reader's own type.
  const question = () => toolName(props.name) === "question";
  const answer = () =>
    question() && props.result && !props.result.is_error ? props.result.output.trim() : "";
  // An MCP tool whose server pairs it with a page: the page shows under the
  // row, once its server's apps are known to be on. With them off the row is
  // the plain tool, also for a result from before they were turned off.
  const store = useApp();
  const connection = () => store.state.connection?.id ?? "";
  const payload = createMemo<McpAppPayload | undefined>(
    () => (props.nested || props.result?.is_error ? undefined : mcpAppPayload(props.name, props.result?.metadata)),
    undefined,
    {
      equals: (a, b) =>
        a === b || (!!a && !!b && a.server === b.server && a.uri === b.uri && a.result === b.result),
    },
  );
  const off = () => {
    const value = payload();
    if (!value) return false;
    loadMcpServers(connection(), store.state.catalogVersion, !store.state.connection?.remote);
    return !mcpServersKnown(connection()) || appsOff(connection(), value.server);
  };
  const app = createMemo(() => (off() ? undefined : payload()));
  // A read that returned an image shows it once its row is open.
  const image = createMemo(
    () =>
      readToolImage(
        toolName(props.name),
        props.result,
        store.state.connection ? projectRoots(store.state.connection) : [],
      ),
    undefined,
    {
      equals: (a, b) =>
        a === b ||
        (!!a &&
          !!b &&
          ("data" in a
            ? "data" in b && a.data === b.data
            : "path" in b && a.path === b.path && a.size === b.size)),
    },
  );
  /** This row's own menu turned the apps off: it says so where the page was, with Undo. */
  const [turnedOff, setTurnedOff] = createSignal(false);
  const apps = (on: boolean) => {
    const server = payload()?.server;
    if (!server) return;
    setTurnedOff(!on);
    void store.run(() => setApps(connection(), server, on));
  };
  return (
    <>
    <Show when={toolName(props.name) === "preview" && props.result && !props.result.is_error && previewInfo(props.result.metadata)} fallback={<details
      class="tool-call"
      classList={{
        "tool-child": props.nested,
        "tool-batch": !!batch().length,
        "tool-question": question(),
      }}
      data-status={status()}
      open={open()}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary
        // A drag that selects the command or path is for copying, not for opening the row.
        onPointerDown={(event) => (pressed = markSelection(event))}
        onClick={(event) => {
          if (selectedByClick(event.currentTarget, event, pressed)) event.preventDefault();
          pressed = undefined;
        }}
      >
        <span class="tool-mark" aria-hidden="true">
          <Icon name={shape().icon} size="sm" />
        </span>
        {/* A step in motion sweeps like the activity line and the agent card;
            a finished one simply rests. The word stays for screen readers. */}
        <Shimmer class="tool-label" active={live()}>
          {/* Held for approval, it has not started: no verb says it runs.
              The icon still says what kind of step it is. */}
          <Show when={!waiting()}>
            <strong>{verb()}</strong>
          </Show>
          <span class="tool-target" title={toolTitle(props.args, shape().target)}>
            <SpelledText text={shape().target} />
          </span>
          <Show when={answer()}>
            <span class="tool-answer" title={answer()}>
              {answer()}
            </span>
          </Show>
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
        <Show when={exit()}>
          {(code) => <span class="tool-chip tool-exit">{t("exit {code}", { code: String(code()) })}</span>}
        </Show>
        <Show when={ranInChrome(props.name, props.args, props.result)}>
          <span class="tool-chip">{t("Chrome")}</span>
        </Show>
        <Show when={answeredByHook(props.name, props.result)}>
          <span class="tool-chip">{t("Answered by hook")}</span>
        </Show>
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
                        {t(input().label)}
                        <Show when={input().context}>
                          <span>{input().context}</span>
                        </Show>
                      </div>
                      <Show when={input().text}>
                        <pre class="tool-input">
                          {/* What the call was given reads as on an approval
                              card: what would not show is a quiet code. */}
                          <SpelledOut text={input().text} />
                        </pre>
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
                      <Show when={image()} keyed>
                        {(found) => <ReadImage image={found} alt={imageName(props.args)} />}
                      </Show>
                    </div>
                  }
                >
                  <For each={changes()}>
                    {(change, index) => (
                      <ChangePreview
                        change={change}
                        showPath={index() > 0 || changes().length > 1}
                        review={
                          // Once the change is made, the whole file is a
                          // click away in Review.
                          props.result && !props.result.is_error && change.path
                            ? () => {
                                requestReview(connection(), change.path);
                                store.set("panel", "diff");
                              }
                            : undefined
                        }
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
    <Show when={app()} keyed>
      {(value) => (
        <McpAppView
          payload={value}
          arguments={props.args}
          callKey={props.id ?? value.uri}
          turnOff={
            // A remote host's servers are changed on that host, and a
            // plugin's in the plugin.
            store.state.connection?.remote || readOnlyServer(connection(), value.server)
              ? undefined
              : () => apps(false)
          }
        />
      )}
    </Show>
    <Show when={turnedOff() && payload() && off() && mcpServersKnown(connection())}>
      <div class="mcp-app mcp-app-off">
        <div class="notice-inline mcp-app-notice" role="status">
          <Icon name="eye-off" size="sm" />
          <span>
            {t("Interactive apps are off for {server}. Its tools show as text.", {
              server: payload()!.server,
            })}
          </span>
          <button class="mcp-app-action" onClick={() => apps(true)}>
            {t("Undo")}
          </button>
        </div>
      </div>
    </Show>
    </>
  );
}
/**
 * The image a read returned, inside its open row: the one its result
 * carries, or the project file it read, read again like a reply's image.
 * Nothing shows until the image is there; shown smaller than it is, a click
 * opens it full size.
 */
function ReadImage(props: { image: { data: string } | { path: string; size?: number }; alt: string }) {
  const store = useApp();
  const image = props.image;
  const project = imageProject(store.state);
  /** What the project's images are remembered under (in This computer, the chat's folder). */
  const connection = project?.scope;
  /** The file as the read saw it: one of another size is not shown. */
  const same = (shown: ShownImage | undefined) =>
    shown?.data !== undefined && ("data" in image || image.size === undefined || shown.size === image.size)
      ? shown.data
      : undefined;
  const remembered = "path" in image && connection ? shownProjectImage(connection, image.path) : undefined;
  const [data, setData] = createSignal("data" in image ? image.data : same(remembered));
  if ("path" in image && project) {
    // A remembered picture of another size may be from before the read, or
    // after it: the file is read again at once, as if it had been written.
    if (remembered?.data !== undefined && !same(remembered)) projectFileWritten(project.scope, image.path);
    const reading = new AbortController();
    const off = onProjectImageSettled((scope, path, shown) => {
      // A picture the row shows stays when its file changes or goes.
      const next = scope === connection && path === image.path ? same(shown) : undefined;
      if (next) setData(next);
    });
    readProjectImage(project, image.path, reading.signal);
    onCleanup(() => {
      reading.abort();
      off();
    });
  }
  return (
    <Show when={data()}>
      {(src) => (
        <div
          class="tool-image"
          ref={(element) => {
            // Shown smaller than it is, it opens full size.
            const opening = watchOpenImages(element, () => t("Image"));
            onCleanup(opening.dispose);
          }}
        >
          <img src={src()} alt={props.alt} />
        </div>
      )}
    </Show>
  );
}
export function ChangePreview(props: {
  change: Change;
  showPath?: boolean;
  /** Opens the change's file in Review. */
  review?: () => void;
  /** On an approval card: a direction override or isolate in a line reads
   * as a quiet code, so the change cannot read as something it is not. */
  directions?: boolean;
}) {
  const rows = createMemo(() => changeRows(props.change));
  const shown = () => rows().slice(0, CHANGE_PREVIEW_LINES);
  const hidden = () => rows().length - shown().length;
  return (
    <div class="change-preview">
      <Show when={props.showPath && props.change.path}>
        <div class="change-path">
          <SpelledOut text={props.change.path} />
        </div>
      </Show>
      <div class="change-lines">
        <For each={shown()}>
          {(row) => (
            <div class={"diff-line " + row.kind}>
              {/* A quiet gutter says what colour alone would: added or
                  removed. It is left out of a copied selection. */}
              <span class="diff-sign">
                {row.kind === "add" ? "+" : row.kind === "remove" ? "−" : " "}
              </span>
              <code>
                <Show when={props.directions} fallback={row.text || " "}>
                  <SpelledOut text={row.text || " "} shows={turnsDirection} />
                </Show>
              </code>
            </div>
          )}
        </For>
      </div>
      <Show
        when={props.review}
        fallback={
          <Show when={hidden() > 0}>
            <p class="change-more">
              {t("{count} more lines · open Review to see all", { count: hidden() })}
            </p>
          </Show>
        }
      >
        {(review) => (
          <p class="change-more change-review">
            <Show when={hidden() > 0}>
              <span>{t("{count} more lines", { count: hidden() })}</span>
            </Show>
            <button type="button" class="change-review-link" onClick={() => review()()}>
              {t("Show in Review")}
            </button>
          </p>
        )}
      </Show>
    </div>
  );
}
