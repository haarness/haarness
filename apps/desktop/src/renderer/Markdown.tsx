import { t } from "./i18n";
import { useApp } from "./state";
import { createEffect, onCleanup } from "solid-js";
import { createMarkdownClient } from "./markdown-client";
import { createMarkdownDOM } from "./markdown-dom";
import { linkTarget } from "./link-target";

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
  let host!: HTMLDivElement;
  let patch: ReturnType<typeof createMarkdownDOM> | undefined;
  let disposed = false;
  const timers = new Set<ReturnType<typeof setTimeout>>();
  const client = createMarkdownClient((update, live) => {
    if (disposed) return;
    (patch ??= createMarkdownDOM(host))(update);
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
