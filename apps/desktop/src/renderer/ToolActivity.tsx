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
import { imageProject, onProjectImageSettled, readProjectImage, readToolImage, shownProjectImage } from "./project-images";
import { openImage } from "./ImageViewer";
import { Icon } from "./ui";
import { Shimmer } from "./Shimmer";
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
  toolInput,
  toolName,
  toolOutput,
  toolTitle,
  toolFailed,
  toolVerb,
  ranInChrome,
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
  const verb = () => {
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
    () => props.result && toolOutput(props.name, props.result.output),
  );
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
    () => readToolImage(toolName(props.name), props.result, store.state.connection?.workspace ?? ""),
    undefined,
    {
      equals: (a, b) =>
        a === b || (!!a && !!b && ("data" in a ? "data" in b && a.data === b.data : "path" in b && a.path === b.path)),
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
          <strong>{verb()}</strong>
          <span class="tool-target" title={toolTitle(props.args, shape().target)}>
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
                      <Show when={image()} keyed>
                        {(found) => <ReadImage image={found} alt={(shape().target ?? "").split(/[\\/]/).pop() ?? ""} />}
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
 * Nothing shows until the image is there; a click opens it full size.
 */
function ReadImage(props: { image: { data: string } | { path: string }; alt: string }) {
  const store = useApp();
  const image = props.image;
  const project = imageProject(store.state);
  const connection = project?.connection;
  const [data, setData] = createSignal(
    "data" in image ? image.data : connection ? shownProjectImage(connection, image.path)?.data : undefined,
  );
  if ("path" in image && project) {
    const reading = new AbortController();
    const off = onProjectImageSettled((scope, path, shown) => {
      if (scope === connection && path === image.path && shown.data !== undefined) setData(shown.data);
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
        <div class="tool-image">
          <button
            type="button"
            class="tool-image-open"
            data-image-open=""
            aria-haspopup="dialog"
            aria-label={props.alt || t("Image")}
            onClick={(event) => openImage(event.currentTarget)}
          >
            <img src={src()} alt={props.alt} />
          </button>
        </div>
      )}
    </Show>
  );
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
