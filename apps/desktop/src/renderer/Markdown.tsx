import { language, t } from "./i18n";
import { useApp } from "./state";
import { createEffect, on, onCleanup, onMount } from "solid-js";
import { createMarkdownClient } from "./markdown-client";
import { createMarkdownDOM } from "./markdown-dom";
import { linkTarget } from "./link-target";
import {
  imageLoading,
  loadableSources,
  loadedElement,
  loadedImage,
  loadImage,
  madeHere,
  onImageSettled,
  relabelImages,
} from "./remote-images";
import {
  noteImage,
  onProjectImageSettled,
  projectImageElement,
  imageProject,
  projectRoots,
  readProjectImages,
  relabelProjectImages,
  shownProjectImage,
  type ShownImage,
} from "./project-images";
import { watchOpenImages } from "./ImageViewer";

/**
 * What is read in the transcript's view: the first text that starts inside
 * it, under its middle. Starting inside the view, it holds nothing above it,
 * so an image growing there moves it whole; an image or a placeholder, which
 * may be the one about to come in, is passed over.
 */
function readingAnchor(list: HTMLElement, view: DOMRect) {
  for (let y = view.top + 1; y < view.bottom; y += 12) {
    const point = document.elementFromPoint(view.left + view.width / 2, y);
    if (!(point instanceof HTMLElement) || point === list || !list.contains(point)) continue;
    if (point instanceof HTMLImageElement || point.closest(".project-image, .remote-image")) continue;
    if (point.getBoundingClientRect().top >= view.top) return point;
  }
  return undefined;
}

/** Transcripts with a correction of the reading position waiting for its rows to be measured. */
const correcting = new WeakSet<HTMLElement>();

/**
 * Makes `change` (an image coming into `host`) without moving what is being
 * read when `element` is above the top of the transcript's view. The
 * transcript turns the browser's scroll anchoring off; its virtualizer keeps
 * a row in place only when the whole row is above the view, so a reply that
 * reaches into the view, or the live reply outside the rows, would move by
 * as much as the image grew. So what is read (`readingAnchor`, or else the
 * end of this reply) is noted before, and once the rows are measured (the
 * virtualizer's observer was made first, so it hears first, and any place it
 * kept is counted) and before they are painted, the view moves by as much as
 * that moved. One correction waits per transcript: images coming in while it
 * waits, in this reply or another (your own message showing the same image),
 * are counted in it. The image is decoded before it comes in, so it takes its
 * final size at once.
 */
function keepReading(element: HTMLElement, host: HTMLElement, change: () => void) {
  const list = element.closest<HTMLElement>(".transcript");
  const view = list?.getBoundingClientRect();
  if (!list || !view || element.getBoundingClientRect().top >= view.top || correcting.has(list)) return change();
  const anchor = readingAnchor(list, view);
  const where = () =>
    (anchor ? anchor.getBoundingClientRect().top : host.getBoundingClientRect().bottom) -
    list.getBoundingClientRect().top;
  const before = where();
  correcting.add(list);
  change();
  const measured = new ResizeObserver(() => {
    measured.disconnect();
    correcting.delete(list);
    if (anchor && !anchor.isConnected) return;
    const moved = where() - before;
    if (moved) list.scrollTop += moved;
  });
  measured.observe(host);
}

/**
 * `document` renders a Markdown file, where a lone newline is not a line
 * break. `base` is the folder its relative links start from (the file's own
 * folder); a reply's links start from the project.
 */
export function Markdown(props: {
  text: string;
  live?: boolean;
  document?: boolean;
  base?: string;
  /** Takes every link instead of the usual web, file and chat handling. */
  onLink?: (href: string) => void;
}) {
  const app = useApp();
  /**
   * In the conversation an image whose source is a path shows that file of
   * the project, read through the server; a Markdown file's preview and
   * release notes draw it as before.
   */
  const conversation = !props.document;
  /** Web links leave through the desktop's link check; file links open in the Files panel. */
  function follow(href: string) {
    const connection = app.state.connection;
    const thread = app.state.selected;
    const target = linkTarget(href, { root: connection?.workspace ?? "", base: props.base });
    if (target.kind === "web") {
      void window.haarness
        .openLink(connection?.id, thread, target.url)
        .catch(() => app.notify(t("This link could not be opened."), "blocked"));
    } else if (target.kind === "file") {
      if (!connection) return app.notify(t("Open a project to open links to its files."), "blocked");
      app.set("fileRequest", {
        connection: connection.id,
        path: target.path,
        sequence: Date.now(),
        thread: thread || undefined,
        link: true,
        ...(target.line ? { line: target.line } : {}),
      });
    } else if (target.kind === "refused")
      app.notify(
        target.reason === "network"
          ? t("Links to network locations do not open.")
          : target.reason === "outside"
            ? t("Only files in this project open from a link.")
            : t("This kind of link does not open in Haarness."),
        "blocked",
      );
  }
  /** Images from another host are remembered for the chat they were loaded in. */
  const scope = () => `${app.state.connection?.id ?? ""}\n${app.state.selected ?? ""}`;
  /**
   * The person asked for the image this button names: it, and the reply's
   * other images from the same host, are fetched by the app and replace
   * their placeholders. Images from other hosts keep waiting for their own click.
   * Only placeholders the app made count, and only addresses on that host.
   */
  async function loadImages(button: HTMLButtonElement) {
    const holder = button.closest<HTMLElement>(".remote-image");
    const from = holder?.dataset.remoteHost;
    if (!holder || !from || !madeHere(holder) || !madeHere(button)) return;
    const focused = document.activeElement === button;
    const placeholders = () =>
      Array.from(host.querySelectorAll<HTMLElement>(".remote-image")).filter((other) => madeHere(other));
    const sources = loadableSources(
      placeholders().map((other) => ({ host: other.dataset.remoteHost, src: other.dataset.remoteSrc })),
      from,
    );
    const chat = scope();
    for (const other of placeholders()) {
      const load = other.querySelector<HTMLButtonElement>(".remote-image-load");
      if (other.dataset.remoteHost === from && load && madeHere(load)) load.disabled = true;
    }
    // Each fetch is kept with the chat, not in this reply: every mounted reply
    // swaps in, or turns back on, its own placeholders when it settles
    // (onImageSettled below), so one drawn anew meanwhile is not left waiting.
    const results = await Promise.allSettled(
      sources.map((src) => loadImage(chat, src, (address) => window.haarness.remoteImage(address))),
    );
    if (disposed) return;
    if (focused && !holder.isConnected) {
      // The button is gone, so keyboard focus stays on what replaced it.
      const image = Array.from(host.querySelectorAll<HTMLImageElement>("img.remote-image-loaded")).find(
        (other) => other.dataset.remoteSrc === holder.dataset.remoteSrc,
      );
      if (image) {
        image.tabIndex = -1;
        image.focus({ preventScroll: true });
      }
    }
    if (results.some((result) => result.status === "rejected"))
      app.notify(t("The image from {host} could not be loaded.", { host: from }), "blocked");
  }
  /** A fetch for this chat settled: this reply's placeholders for it show the image, or take clicks again. */
  function settled(chat: string, src: string, data: string | undefined) {
    if (disposed || !host || chat !== scope()) return;
    for (const holder of Array.from(host.querySelectorAll<HTMLElement>(".remote-image"))) {
      if (!madeHere(holder) || holder.dataset.remoteSrc !== src) continue;
      if (data) {
        holder.replaceWith(loadedElement(document, src, holder.dataset.remoteAlt ?? "", data));
        continue;
      }
      const load = holder.querySelector<HTMLButtonElement>(".remote-image-load");
      if (load && madeHere(load)) load.disabled = false;
    }
    opening?.refresh();
  }
  onCleanup(onImageSettled(settled));
  const imageLabels = () => ({
    load: (from: string) => t("Load image from {host}", { host: from }),
    unnamed: t("Image"),
  });
  const projectLabels = () => ({
    unnamed: t("Image"),
    outside: t("Outside this project"),
    large: t("Too large to show"),
    failed: t("Not available"),
  });
  /** Reads end with the reply: one still waiting for a slot is never started. */
  const reading = new AbortController();
  const asked = new Set<string>();
  /** A read for this project settled: this reply's placeholders for the path show the image, or why not. */
  function projectSettled(connection: string, path: string, shown: ShownImage) {
    if (disposed || !host || connection !== app.state.connection?.id) return;
    const elements = Array.from(host.querySelectorAll<HTMLElement>(".project-image, img.project-image-loaded")).filter(
      (element) => madeHere(element) && element.dataset.projectPath === path,
    );
    if (!elements.length) return;
    keepReading(elements[0], host, () => {
      for (const element of elements) {
        if (shown.data === undefined) {
          if (!(element instanceof HTMLImageElement)) noteImage(element, shown.note, projectLabels(), path);
        } else if (element instanceof HTMLImageElement) {
          if (element.getAttribute("src") !== shown.data) element.src = shown.data;
        } else element.replaceWith(projectImageElement(document, path, element.dataset.projectAlt ?? "", shown.data));
      }
    });
    opening?.refresh();
  }
  if (conversation) onCleanup(onProjectImageSettled(projectSettled));
  let host!: HTMLDivElement;
  let opening: ReturnType<typeof watchOpenImages> | undefined;
  let patch: ReturnType<typeof createMarkdownDOM> | undefined;
  let disposed = false;
  const timers = new Set<ReturnType<typeof setTimeout>>();
  const client = createMarkdownClient((update, live) => {
    if (disposed) return;
    (patch ??= createMarkdownDOM(host, {
      loaded: (src) => loadedImage(scope(), src),
      loading: (src) => imageLoading(scope(), src),
      labels: imageLabels,
      project: conversation
        ? {
            root: () => (app.state.connection ? projectRoots(app.state.connection) : []),
            shown: (path) => {
              const connection = app.state.connection?.id;
              const now = connection ? shownProjectImage(connection, path) : undefined;
              if (!now?.note) return now;
              // A picture this reply already shows stays when its file is
              // gone; only a place still waiting says why.
              const live = Array.from(host.querySelectorAll<HTMLImageElement>("img.project-image-loaded")).find(
                (image) => madeHere(image) && image.dataset.projectPath === path,
              );
              return live ? { data: live.getAttribute("src")! } : now;
            },
            labels: projectLabels,
          }
        : undefined,
    }))(update);
    if (conversation) readProjectImages(host, imageProject(app.state), reading.signal, asked, projectLabels());
    opening?.refresh();
    for (const block of host.querySelectorAll("pre")) {
      const code = block.querySelector("code");
      const language = /language-([\w+#-]+)/.exec(code?.className ?? "")?.[1];
      // "text" and "plaintext" say nothing a reader needs; label real languages only.
      if (language && !/^(text|plain|plaintext|txt|none|output|console)$/i.test(language))
        block.dataset.language = language;
      else delete block.dataset.language;
      if (live) { block.querySelector(":scope > .code-copy")?.remove(); continue; }
      if (block.querySelector(":scope > .code-copy")) continue;
      const button = document.createElement("button");
      button.type = "button";
      button.className = "code-copy";
      button.setAttribute("aria-label", t("Copy code"));
      button.title = t("Copy code");
      button.textContent = t("Copy");
      block.append(button);
    }
    host.dataset.markdownReady = "true";
    host.dispatchEvent(new CustomEvent("markdown-ready", { bubbles: true }));
  });
  createEffect(() => {
    host.dataset.markdownReady = "false";
    client.update(props.text, !!props.live, !props.document);
  });
  // Asked when something waits on this text being drawn (a selection kept
  // from the live reply it replaces).
  const drawNow = () => client.flush();
  onMount(() => host.addEventListener("markdown-draw-now", drawNow));
  onCleanup(() => host?.removeEventListener("markdown-draw-now", drawNow));
  // Image placeholders already drawn follow a language change.
  createEffect(
    on(
      language,
      () => {
        relabelImages(host, imageLabels());
        relabelProjectImages(host, projectLabels());
      },
      { defer: true },
    ),
  );
  onCleanup(() => {
    disposed = true;
    reading.abort();
    client.dispose();
    timers.forEach(clearTimeout);
  });
  // An image of the conversation shown smaller than it is opens full size.
  onMount(() => {
    if (conversation) opening = watchOpenImages(host, () => t("Image"));
  });
  onCleanup(() => opening?.dispose());
  return (
    <div
      ref={host}
      class={props.document ? "markdown markdown-document" : "markdown"}
      onClick={(event) => {
        const target = event.target as HTMLElement;
        const load = target.closest<HTMLButtonElement>(".remote-image-load");
        if (load && host.contains(load) && madeHere(load)) {
          // A link around the image must not take the click meant for its button.
          event.preventDefault();
          void loadImages(load);
          return;
        }
        const button = target.closest<HTMLButtonElement>(".code-copy");
        if (button && host.contains(button)) {
          const code = button.parentElement?.querySelector("code");
          void navigator.clipboard.writeText(code?.textContent ?? "").then(() => {
            if (disposed || !button.isConnected) return;
            button.textContent = t("Copied");
            button.classList.add("done");
            const timer = setTimeout(() => {
              timers.delete(timer);
              button.textContent = t("Copy");
              button.classList.remove("done");
            }, 1600);
            timers.add(timer);
          }).catch(() => {});
          return;
        }
        const link = target.closest("a");
        if (link) {
          event.preventDefault();
          // The written address: `link.href` would resolve a path against this window's own file.
          const href = link.getAttribute("href") ?? "";
          if (props.onLink) props.onLink(href);
          else follow(href);
        }
      }}
    />
  );
}
