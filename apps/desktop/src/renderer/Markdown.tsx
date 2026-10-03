import { useApp } from "./state";
import { createEffect, onCleanup } from "solid-js";
import { createMarkdownClient } from "./markdown-client";
import { createMarkdownDOM } from "./markdown-dom";

export function Markdown(props: { text: string; live?: boolean }) {
  const app = useApp();
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
      button.setAttribute("aria-label", "Copy code");
      button.title = "Copy code";
      button.textContent = "Copy";
      block.append(button);
    }
    host.dataset.markdownReady = "true";
    host.dispatchEvent(new CustomEvent("markdown-ready", { bubbles: true }));
  });
  createEffect(() => {
    host.dataset.markdownReady = "false";
    client.update(props.text, !!props.live);
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
            button.textContent = "Copied";
            button.classList.add("done");
            const timer = setTimeout(() => {
              timers.delete(timer);
              button.textContent = "Copy";
              button.classList.remove("done");
            }, 1600);
            timers.add(timer);
          }).catch(() => {});
          return;
        }
        const link = target.closest("a");
        if (link) {
          event.preventDefault();
          const connection = app.state.connection?.id, thread = app.state.selected;
          void window.haarness.openLink(connection, thread, link.href).catch((error) => app.notify(String(error)));
        }
      }}
    />
  );
}
