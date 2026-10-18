import { t } from "./i18n";
import { useApp } from "./state";
import { createEffect, onCleanup } from "solid-js";
import { createMarkdownClient } from "./markdown-client";
import { createMarkdownDOM } from "./markdown-dom";
import { linkTarget } from "./link-target";
import { loadedElement, loadedImage, rememberImage } from "./remote-images";

/**
 * `document` renders a Markdown file, where a lone newline is not a line
 * break. `base` is the folder its relative links start from (the file's own
 * folder); a reply's links start from the project.
 */
export function Markdown(props: { text: string; live?: boolean; document?: boolean; base?: string }) {
  const app = useApp();
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
   */
  async function loadImages(button: HTMLButtonElement) {
    const holder = button.closest<HTMLElement>(".remote-image");
    const from = holder?.dataset.remoteHost;
    if (!holder || !from) return;
    const focused = document.activeElement === button;
    const sources = [
      ...new Set(
        Array.from(host.querySelectorAll<HTMLElement>(".remote-image"))
          .filter((other) => other.dataset.remoteHost === from)
          .map((other) => other.dataset.remoteSrc ?? ""),
      ),
    ].filter(Boolean);
    const chat = scope();
    const waiting = () =>
      Array.from(host.querySelectorAll<HTMLButtonElement>(".remote-image-load")).filter(
        (other) => other.closest<HTMLElement>(".remote-image")?.dataset.remoteHost === from,
      );
    for (const other of waiting()) other.disabled = true;
    let failed = false;
    await Promise.all(
      sources.map(async (src) => {
        try {
          const data = await window.haarness.remoteImage(src);
          rememberImage(chat, src, data);
          if (disposed) return;
          // Placeholders are found again: the reply may have been drawn anew meanwhile.
          for (const other of Array.from(host.querySelectorAll<HTMLElement>(".remote-image"))) {
            if (other.dataset.remoteSrc !== src) continue;
            const image = loadedElement(document, src, other.dataset.remoteAlt ?? "", data);
            const keep = focused && other === holder;
            other.replaceWith(image);
            if (keep) {
              // The button is gone, so keyboard focus stays on what replaced it.
              image.tabIndex = -1;
              image.focus({ preventScroll: true });
            }
          }
        } catch {
          failed = true;
        }
      }),
    );
    if (disposed) return;
    for (const other of waiting()) other.disabled = false;
    if (failed) app.notify(t("The image from {host} could not be loaded.", { host: from }), "blocked");
  }
  let host!: HTMLDivElement;
  let patch: ReturnType<typeof createMarkdownDOM> | undefined;
  let disposed = false;
  const timers = new Set<ReturnType<typeof setTimeout>>();
  const client = createMarkdownClient((update, live) => {
    if (disposed) return;
    (patch ??= createMarkdownDOM(host, {
      loaded: (src) => loadedImage(scope(), src),
      labels: () => ({
        load: (from) => t("Load image from {host}", { host: from }),
        unnamed: t("Image"),
      }),
    }))(update);
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
  onCleanup(() => {
    disposed = true;
    client.dispose();
    timers.forEach(clearTimeout);
  });
  return (
    <div
      ref={host}
      class="markdown"
      onClick={(event) => {
        const target = event.target as HTMLElement;
        const load = target.closest<HTMLButtonElement>(".remote-image-load");
        if (load && host.contains(load)) {
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
          follow(link.getAttribute("href") ?? "");
        }
      }}
    />
  );
}
