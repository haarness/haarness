import type { ReviewDraft } from "../review-drafts";
import { t } from "./i18n";
import { Select } from "./Select";
import {
  For,
  Show,
  createSignal,
  createEffect,
  createMemo,
  createResource,
  untrack,
  on,
  onMount,
  onCleanup,
} from "solid-js";
import type { FileDiff } from "@haarness/sdk";
import { diffWordsWithSpace } from "diff";
import { useApp } from "./state";
import { Button, Icon, Empty } from "./ui";
import { ReviewRows } from "./ReviewRows";
import { ReviewCache } from "./review-cache";
import { sessionKey } from "../projects";
import { lines, split, patchText, type DiffLine } from "./diff";
export function Review(props: { compact?: boolean } = {}) {
  const app = useApp();
  let root!: HTMLElement;
  const [scroll, setScroll] = createSignal<HTMLElement>();
  const [tree, setTree] = createSignal<HTMLElement>();
  const [filter, setFilter] = createSignal("");
  const [revealFile, setRevealFile] = createSignal<number>();
  const [loading, setLoading] = createSignal(false);
  const [cache, setCache] = createSignal<ReviewCache>();
  const [active, setActive] = createSignal("");
  const [drafts, setDrafts] = createSignal<Record<string, ReviewDraft>>({});
  const [draftsReady, setDraftsReady] = createSignal(false);
  const scope = () => JSON.stringify([app.state.connection?.id, app.state.selected, mode()]);
  let draftGeneration = 0;
  let disposed = false;
  onCleanup(() => { disposed = true; draftGeneration++; });
  async function saveDraft(key: string, text: string) {
    const draft = { id: crypto.randomUUID(), text };
    setDrafts(values => ({ ...values, [key]: draft }));
    await window.haarness.review({ action: "save", scope: scope(), anchor: key, draft });
  }
  const [showFiles, setShowFiles] = createSignal(!props.compact);
  const [mode, setMode] = createSignal("working"),
    [context, setContext] = createSignal(4),
    [files, setFiles] = createSignal<FileDiff[]>([]),
    [selected, setSelected] = createSignal(""),
    [reviewed, setReviewed] = createSignal<string[]>([]),
    [problem, setProblem] = createSignal(""),
    [all, setAll] = createSignal(false);
  createEffect(
    on(
      scope,
      (owner) => {
        const generation = ++draftGeneration;
        setActive("");
        setDrafts({});
        setDraftsReady(false);
        setReviewed([]);
        void app.run(async () => {
          const saved = await window.haarness.review({ action: "load", scope: owner });
          if (disposed || generation !== draftGeneration) return;
          setDrafts(saved.drafts);
          setDraftsReady(true);
        });
      },
    ),
  );
  let generation = 0;
  onCleanup(() => {
    generation++;
    cache()?.dispose();
  });
  async function refresh() {
    const id = ++generation;
    const source = mode(),
      connection = app.state.connection?.id,
      thread = app.state.selected,
      contextLines = context();
    if (!connection) return;
    cache()?.dispose();
    setCache(undefined);
    setLoading(true);
    setProblem("");
    setFiles([]);
    const read = (summary: boolean, path?: string) =>
      source === "last"
        ? window.haarness.api<FileDiff[]>(connection, "lastTurnReview", [
            thread,
            { summary, path },
          ])
        : window.haarness.api<FileDiff[]>(connection, "vcsDiff", [
            { mode: source, context: contextLines, summary, path },
          ]);
    try {
      const data = await read(true);
      if (id !== generation) return;
      const next = new ReviewCache(async (path) =>
        (await read(false, path)).find((file) => file.file === path),
      );
      setCache(next);
      setFiles(data);
      setReviewed((values) =>
        values.filter((path) => data.some((file) => file.file === path)),
      );
      if (!data.some((file) => file.file === selected()))
        setSelected(data[0]?.file ?? "");
      if (scroll()) scroll()!.scrollTop = 0;
    } catch (error) {
      if (id !== generation) return;
      setProblem(error instanceof Error ? error.message : String(error));
    } finally {
      if (id === generation) setLoading(false);
    }
  }
  const noRepository = () =>
    /not (?:inside a git worktree|a git repository)|workspace is not a directory/i.test(
      problem(),
    );
  app.registerSearch("review", () => [
    {
      key: "review-refresh",
      group: "Review actions",
      name: t("Refresh changes"),
      icon: "refresh",
      action: refresh,
    },
    {
      key: "review-layout",
      group: "Review actions",
      name:
        app.state.preferences.diffLayout === "split"
          ? "Use unified diff"
          : "Use split diff",
      keywords: "review layout side by side",
      icon: "diff",
      action: () =>
        app.settings({
          diffLayout:
            app.state.preferences.diffLayout === "split" ? "unified" : "split",
        }),
    },
    ...["working", "branch", "last"].map((value) => ({
      key: "review-source:" + value,
      group: "Review actions",
      name:
        value === "last"
          ? "Review last turn"
          : value === "branch"
            ? "Review current branch"
            : "Review working changes",
      icon: "diff" as const,
      disabled: value === "last" && !app.state.selected,
      action: () => setMode(value),
    })),
  ]);
  createEffect(() => {
    app.state.connection?.id;
    if (mode() === "last") app.state.selected;
    mode();
    context();
    untrack(() => void refresh());
  });
  const filtered = createMemo(() =>
    files().filter((file) =>
      file.file.toLowerCase().includes(filter().toLowerCase()),
    ),
  );
  const shown = createMemo(() =>
    all() ? filtered() : files().filter((file) => file.file === selected()),
  );
  createEffect(
    on([selected, all, filter], () => {
      if (scroll()) scroll()!.scrollTop = 0;
    }),
  );
  createEffect(
    on(filter, () => {
      if (tree()) tree()!.scrollTop = 0;
      setRevealFile(undefined);
    }),
  );
  function chooseFile(path: string) {
    setSelected(path);
    if (all()) setRevealFile(shown().findIndex((file) => file.file === path));
  }
  const anchor = (
    file: FileDiff,
    row: DiffLine,
    side: "original" | "updated",
  ) =>
    JSON.stringify([file.file, side, side === "original" ? row.old : row.line]);
  function start(key: string) {
    if (!draftsReady()) return;
    setActive(key);
    root.querySelector<HTMLTextAreaElement>(".diff-comment textarea")?.focus();
  }
  function close(key: string, id = drafts()[key]?.id, owner = scope()) {
    if (id) void app.run(() => window.haarness.review({ action: "remove", scope: owner, anchor: key, id }));
    if (disposed || scope() !== owner || (drafts()[key] && drafts()[key].id !== id)) return;
    const current = active() === key;
    if (current) setActive("");
    setDrafts((values) => {
      const next = { ...values };
      delete next[key];
      return next;
    });
    if (current)
      root
        .querySelector<HTMLButtonElement>(
          `[data-comment-anchor="${CSS.escape(key)}"]`,
        )
        ?.focus({ preventScroll: true });
  }
  async function comment(
    file: FileDiff,
    row: DiffLine,
    side: "original" | "updated",
    value: string,
  ) {
    const owner = scope();
    const revision = draftGeneration;
    const anchorKey = anchor(file, row, side);
    const saved = drafts()[anchorKey];
    if (!saved || saved.text.trim() !== value) throw new Error("Review comment changed; try again");
    const connection = app.state.connection?.id;
    if (!connection) throw new Error("Open project first");
    const target = saved.target ?? { connection, thread: app.state.selected || (await app.create()).id };
    const pending = { ...saved, target };
    // Pin retries to the first destination, even after navigation or restart.
    await window.haarness.review({ action: "pin", scope: owner, anchor: anchorKey, draft: pending });
    if (!disposed && scope() === owner && drafts()[anchorKey]?.id === saved.id)
      setDrafts(values => ({ ...values, [anchorKey]: pending }));
    const key = sessionKey(target.connection, target.thread);
    const current = app.state.drafts[key] ?? "";
    const note = `Review comment on ${file.file}:${side === "original" ? row.old : row.line} (${side}):\n${value}`;
    const result = await window.haarness.review({ action: "deliver", scope: owner, anchor: anchorKey, draft: pending, note, current });
    const latest = app.state.drafts[key] ?? "";
    if (latest === current) app.set("drafts", key, result.text ?? current);
    else {
      // Typing while storage completes must survive the acknowledgement.
      const merged = [latest, note].filter(Boolean).join("\n\n");
      app.set("drafts", key, merged);
      await window.haarness.storage("draft:" + target.connection + ":" + target.thread, merged);
    }
    close(anchorKey, saved.id, owner);
    if (!disposed && (revision === draftGeneration || app.state.selected === target.thread)
      && app.state.connection?.id === target.connection && app.state.selected === target.thread) {
      app.set("panel", "chat");
      app.notify("Review comment added to your message");
    }
  }
  function Comment(props: {
    file: FileDiff;
    row?: DiffLine;
    side: "original" | "updated";
    split?: boolean;
  }) {
    const key = () =>
      props.row ? anchor(props.file, props.row, props.side) : "";
    return (
      <Show when={props.row && active() === key()}>
        <div class="diff-comment-row" classList={{ split: props.split }}>
          <InlineComment
            label={`${props.file.file}:${props.side === "original" ? props.row!.old : props.row!.line} (${props.side})`}
            side={props.side}
            value={drafts()[key()]?.text ?? ""}
            onInput={(value) => void app.run(() => saveDraft(key(), value))}
            onCancel={() => close(key())}
            onSubmit={async (value) => {
              await app.run(() => comment(props.file, props.row!, props.side, value));
            }}
          />
        </div>
      </Show>
    );
  }
  function File(props: {
    metadata: FileDiff;
    start: number;
    size: number;
    cache: ReviewCache;
  }) {
    const [data, { refetch }] = createResource(
      () => props.metadata.file,
      (path) => props.cache.load(path),
    );
    const parsed = createMemo(() =>
      !data.error && data() ? lines(data()!) : [],
    );
    const paired = createMemo(() => split(parsed()));
    const reveal = createMemo(() => {
      if (!active()) return undefined;
      const [path, side, line] = JSON.parse(active()) as [
        string,
        string,
        number,
      ];
      if (path !== props.metadata.file) return undefined;
      return app.state.preferences.diffLayout === "split"
        ? paired().findIndex((row) =>
            side === "original"
              ? row.left?.old === line
              : row.right?.line === line,
          )
        : parsed().findIndex((row) =>
            side === "original" ? row.old === line : row.line === line,
          );
    });
    const [offset, setOffset] = createSignal(42);
    let article: HTMLElement | undefined, body: HTMLElement | undefined;
    onMount(() => {
      const update = () => {
        if (article && body)
          setOffset(
            body.getBoundingClientRect().top -
              article.getBoundingClientRect().top,
          );
      };
      const observer = new ResizeObserver(update);
      // Loading resolves after mount; observing the stable wrapper also catches
      // header wrapping and split/unified geometry changes.
      observer.observe(wrapper);
      onCleanup(() => observer.disconnect());
      queueMicrotask(update);
    });
    let wrapper!: HTMLDivElement;
    return (
      <div
        ref={wrapper}
        style={{
          height: data.loading ? `${props.size}px` : undefined,
          overflow: data.loading ? "hidden" : undefined,
        }}
      >
        <Show
          when={!data.error && data()}
          keyed
          fallback={
            <article class="diff-file" data-file={props.metadata.file}>
              <header>
                <strong>{props.metadata.file}</strong>
              </header>
              <p class="notice-inline" role="status">
                {data.loading
                  ? t("Loading changes…")
                  : data.error
                    ? String(data.error)
                    : t("This file no longer has changes. Refresh the review.")}
              </p>
              <Show when={data.error}>
                <button onClick={() => void refetch()}>{t("Retry")}</button>
              </Show>
            </article>
          }
        >
          {(file) => (
            <article
              class="diff-file"
              data-file={file.file}
              ref={(element) => {
                article = element;
              }}
            >
              <header>
                <strong>{file.file}</strong>
                <span class="muted">{file.status}</span>
                <div class="grow" />
                <label>
                  <input
                    type="checkbox"
                    checked={reviewed().includes(file.file)}
                    onChange={(event) =>
                      setReviewed((values) =>
                        event.currentTarget.checked
                          ? [...values, file.file]
                          : values.filter((path) => path !== file.file),
                      )
                    }
                  />
                  {t("Reviewed")}
                </label>
                <Button
                  icon="copy"
                  label={t("Copy patch")}
                  disabled={
                    file.binary ||
                    (!file.patch && file.before == null && file.after == null)
                  }
                  onClick={() => navigator.clipboard.writeText(patchText(file))}
                />
              </header>
              <Show when={file.truncated}>
                <p class="notice-inline">
                  {t("This diff was truncated by the server.")}
                </p>
              </Show>
              <div
                ref={(element) => {
                  body = element;
                }}
              >
                <Show
                  when={!file.binary}
                  fallback={<p class="notice-inline">{t("Binary file changed.")}</p>}
                >
                  <Show
                    when={app.state.preferences.diffLayout === "split"}
                    fallback={
                      <div class="diff-lines">
                        <ReviewRows
                          items={parsed()}
                          scroll={scroll()}
                          offset={props.start + offset()}
                          estimate={23}
                          reveal={reveal()}
                        >
                          {(row) => (
                            <Show
                              when={row.kind !== "hunk"}
                              fallback={<div class="diff-hunk">{row.text}</div>}
                            >
                              <>
                                <div
                                  class={"diff-line " + row.kind}
                                  onDblClick={() =>
                                    start(
                                      anchor(
                                        file,
                                        row,
                                        row.kind === "remove"
                                          ? "original"
                                          : "updated",
                                      ),
                                    )
                                  }
                                >
                                  <button
                                    aria-label={`Comment on ${file.file} line ${row.line ?? row.old ?? 0}`}
                                    data-comment-anchor={anchor(
                                      file,
                                      row,
                                      row.kind === "remove"
                                        ? "original"
                                        : "updated",
                                    )}
                                    aria-expanded={
                                      active() ===
                                      anchor(
                                        file,
                                        row,
                                        row.kind === "remove"
                                          ? "original"
                                          : "updated",
                                      )
                                    }
                                    onClick={() =>
                                      start(
                                        anchor(
                                          file,
                                          row,
                                          row.kind === "remove"
                                            ? "original"
                                            : "updated",
                                        ),
                                      )
                                    }
                                  >
                                    +
                                  </button>
                                  <span class="line-number">
                                    {row.old ?? ""}
                                  </span>
                                  <span class="line-number">
                                    {row.line ?? ""}
                                  </span>
                                  <code>
                                    {row.kind === "add"
                                      ? "+"
                                      : row.kind === "remove"
                                        ? "−"
                                        : " "}
                                    {row.text || " "}
                                  </code>
                                </div>
                                <Comment
                                  file={file}
                                  row={row}
                                  side={
                                    row.kind === "remove"
                                      ? "original"
                                      : "updated"
                                  }
                                />
                                <Show when={row.kind === "context"}>
                                  <Comment
                                    file={file}
                                    row={row}
                                    side="original"
                                  />
                                </Show>
                              </>
                            </Show>
                          )}
                        </ReviewRows>
                      </div>
                    }
                  >
                    <div class="diff-split">
                      <ReviewRows
                        items={paired()}
                        scroll={scroll()}
                        offset={props.start + offset()}
                        estimate={23}
                        reveal={reveal()}
                      >
                        {(row) => (
                          <Show
                            when={!row.hunk}
                            fallback={<div class="diff-hunk">{row.hunk}</div>}
                          >
                            <>
                              <div class="split-row">
                                <DiffCell
                                  row={row.left}
                                  other={row.right}
                                  side="left"
                                  anchor={
                                    row.left &&
                                    anchor(file, row.left, "original")
                                  }
                                  active={
                                    !!row.left &&
                                    active() ===
                                      anchor(file, row.left, "original")
                                  }
                                  comment={(item) =>
                                    start(anchor(file, item, "original"))
                                  }
                                />
                                <DiffCell
                                  row={row.right}
                                  other={row.left}
                                  side="right"
                                  anchor={
                                    row.right &&
                                    anchor(file, row.right, "updated")
                                  }
                                  active={
                                    !!row.right &&
                                    active() ===
                                      anchor(file, row.right, "updated")
                                  }
                                  comment={(item) =>
                                    start(anchor(file, item, "updated"))
                                  }
                                />
                              </div>
                              <Comment
                                file={file}
                                row={row.left}
                                side="original"
                                split
                              />
                              <Comment
                                file={file}
                                row={row.right}
                                side="updated"
                                split
                              />
                            </>
                          </Show>
                        )}
                      </ReviewRows>
                    </div>
                  </Show>
                </Show>
              </div>
            </article>
          )}
        </Show>
      </div>
    );
  }
  return (
    <section ref={root} class="review">
      {/* Keep refresh/source controls available after a transient read failure. */}
      <Show when={!noRepository()}>
        <div class="panel-toolbar">
          <Button
            icon="filter"
            label={t("Toggle changed files")}
            active={showFiles()}
            onClick={() => setShowFiles((value) => !value)}
          />
          <span class="diff-totals">
            <span class="added">
              +{files().reduce((n, file) => n + file.additions, 0)}
            </span>
            <span class="removed">
              −{files().reduce((n, file) => n + file.deletions, 0)}
            </span>
            <span class="muted">
              {files().length} {files().length === 1 ? "file" : "files"}
            </span>
          </span>
          <div class="grow" />
          <Select
            class="pill-select"
            aria-label={t("Diff source")}
            value={mode()}
            onChange={(value) => setMode(value)}
            options={[
              { value: "working", label: "Working tree" },
              { value: "branch", label: "Current branch" },
              {
                value: "last",
                label: "Last turn",
                disabled: !app.state.selected,
              },
            ]}
          />
          <Select
            class="pill-select"
            aria-label={t("Diff layout")}
            value={app.state.preferences.diffLayout}
            onChange={(value) =>
              app.run(() =>
                app.settings({ diffLayout: value as "split" | "unified" }),
              )
            }
            options={[
              { value: "split", label: t("Split") },
              { value: "unified", label: t("Unified") },
            ]}
          />
          <Select
            class="pill-select"
            aria-label={t("Context lines")}
            value={context()}
            onChange={(value) => setContext(Number(value))}
            options={[
              { value: 4, label: "4 lines" },
              { value: 12, label: "12 lines" },
              { value: 50, label: "50 lines" },
              { value: 10000, label: "Full file" },
            ]}
          />
          <Button
            icon="refresh"
            label={t("Refresh changes")}
            onClick={() => void refresh()}
          />
        </div>
      </Show>
      <div class="file-workspace">
        <aside
          class="file-tree review-tree"
          style={{ display: showFiles() ? "flex" : "none" }}
        >
          <label class="check-label diff-all">
            <input
              type="checkbox"
              checked={all()}
              onChange={(event) => {
                setRevealFile(undefined);
                setAll(event.currentTarget.checked);
              }}
            />
            {t("Show every file at once")}
          </label>
          <input
            type="search"
            class="review-filter"
            aria-label={t("Filter changed files")}
            placeholder={t("Filter changed files")}
            value={filter()}
            onInput={(event) => setFilter(event.currentTarget.value)}
          />
          <Show when={showFiles()}>
            <div
              class="review-file-list"
              ref={(element) => queueMicrotask(() => setTree(element))}
            >
              <ReviewRows
                items={filtered()}
                scroll={tree()}
                estimate={28}
                itemKey={(file) => file.file}
              >
                {(file) => (
                  <button
                    class="file-entry"
                    classList={{ selected: selected() === file.file }}
                    onClick={() => chooseFile(file.file)}
                  >
                    <Icon
                      name={reviewed().includes(file.file) ? "check" : "file"}
                      size="sm"
                    />
                    <span title={file.file}>{file.file}</span>
                    <span class="added">+{file.additions}</span>
                    <span class="removed">−{file.deletions}</span>
                  </button>
                )}
              </ReviewRows>
            </div>
          </Show>
        </aside>
        <div
          class="diff-content"
          ref={(element) => queueMicrotask(() => setScroll(element))}
        >
          <Show
            when={files().length}
            fallback={
              <Show
                when={!loading()}
                fallback={
                  <p class="notice-inline" role="status">
                    {t("Loading changed files…")}
                  </p>
                }
              >
                <Show
                  when={problem()}
                  fallback={
                    <Empty
                      title={t("A clean slate.")}
                      detail="No changes in this view. Changes made by you or Haarness will appear here."
                    />
                  }
                >
                  <Show
                    when={noRepository()}
                    fallback={
                      <Empty
                        title={t("Changes are unavailable.")}
                        detail={problem()}
                      />
                    }
                  >
                    <Empty
                      title={t("This folder isn’t a Git repository.")}
                      detail="Review compares your working tree against version control. Initialise a repository to see changes here."
                    >
                      <button onClick={() => void refresh()}>
                        {t("Check again")}
                      </button>
                    </Empty>
                  </Show>
                </Show>
              </Show>
            }
          >
            <Show when={cache()} keyed>
              {(current) => (
                <ReviewRows
                  items={shown()}
                  scroll={scroll()}
                  estimate={180}
                  itemKey={(file) => file.file}
                  reveal={revealFile()}
                >
                  {(file, start, size) => (
                    <File
                      metadata={file}
                      start={start()}
                      size={size()}
                      cache={current}
                    />
                  )}
                </ReviewRows>
              )}
            </Show>
          </Show>
        </div>
      </div>
    </section>
  );
}
function InlineComment(props: {
  label: string;
  side: "original" | "updated";
  value: string;
  onInput: (value: string) => void;
  onCancel: () => void;
  onSubmit: (value: string) => Promise<void>;
}) {
  let input!: HTMLTextAreaElement;
  const [busy, setBusy] = createSignal(false);
  async function submit() {
    const value = props.value.trim();
    if (!value || busy()) return;
    setBusy(true);
    try {
      await props.onSubmit(value);
    } finally {
      setBusy(false);
    }
  }
  onMount(() => {
    const frame = requestAnimationFrame(() => {
      input.focus({ preventScroll: true });
      input.closest(".diff-comment")?.scrollIntoView({ block: "nearest" });
    });
    onCleanup(() => cancelAnimationFrame(frame));
  });
  return (
    <form
      class="diff-comment"
      data-side={props.side}
      aria-label={`Comment on ${props.label}`}
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
      onKeyDown={(event) => {
        if (event.isComposing) return;
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          if (!busy()) props.onCancel();
        }
      }}
    >
      <label>
        <span class="diff-comment-label" title={props.label}>
          {props.label}
        </span>
        <textarea
          ref={input}
          rows={3}
          aria-label={t("Review comment")}
          placeholder={t("Comment on this line…")}
          value={props.value}
          disabled={busy()}
          onInput={(event) => props.onInput(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.isComposing || event.key !== "Enter" || event.shiftKey)
              return;
            event.preventDefault();
            event.stopPropagation();
            void submit();
          }}
        />
      </label>
      <footer>
        <span class="diff-comment-hint">Shift+Enter for a new line</span>
        <button type="button" disabled={busy()} onClick={props.onCancel}>
          {t("Cancel")}
        </button>
        <button
          type="submit"
          class="primary"
          disabled={busy() || !props.value.trim()}
        >
          {t("Add to message")}
        </button>
      </footer>
    </form>
  );
}
function DiffCell(props: {
  row?: DiffLine;
  other?: DiffLine;
  side: string;
  anchor?: string;
  active: boolean;
  comment: (row: DiffLine) => void;
}) {
  const words = createMemo(() => {
    if (
      !props.row ||
      !props.other ||
      props.row.kind === "context" ||
      props.row.text.length + props.other.text.length > 4096
    )
      return null;
    return diffWordsWithSpace(
      props.side === "left" ? props.row.text : props.other.text,
      props.side === "left" ? props.other.text : props.row.text,
    ).filter((part) => (props.side === "left" ? !part.added : !part.removed));
  });
  return (
    <div
      class={"diff-cell " + (props.row?.kind ?? "blank")}
      onDblClick={() => props.row && props.comment(props.row)}
    >
      <button
        aria-label={
          "Comment on " +
          (props.side === "left" ? "original" : "updated") +
          " line " +
          (props.side === "left" ? props.row?.old : props.row?.line)
        }
        data-comment-anchor={props.anchor}
        aria-expanded={props.active}
        disabled={!props.row}
        onClick={() => props.row && props.comment(props.row)}
      >
        +
      </button>
      <span class="line-number">
        {props.side === "left" ? props.row?.old : props.row?.line}
      </span>
      <code>
        <Show when={words()} fallback={props.row?.text ?? " "}>
          <For each={words()!}>
            {(word) => (
              <span classList={{ "word-change": word.added || word.removed }}>
                {word.value}
              </span>
            )}
          </For>
        </Show>
      </code>
    </div>
  );
}
