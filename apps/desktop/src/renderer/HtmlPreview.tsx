import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  onCleanup,
  onMount,
} from "solid-js";
import { Preview } from "./preview";

/** A separate trusted document owns the opaque content frames. The app keeps its strict CSP. */
export function HtmlPreview(props: {
  html: string;
  title?: string;
  fit?: boolean;
}) {
  const id = createUniqueId();
  let revision = 0;
  const document = createMemo(() => {
    const html = props.html;
    const key = `${id}:${++revision}`;
    return { ...Preview.frame(html, { id: key }), id: key };
  });
  let frame!: HTMLIFrameElement;
  let host!: HTMLDivElement;
  const [ready, setReady] = createSignal(false);
  const [width, setWidth] = createSignal(0);
  const [height, setHeight] = createSignal(650);
  const [error, setError] = createSignal("");
  const scale = () => (props.fit && width() ? Math.min(1, width() / 1280) : 1);
  const tall = () => Math.min(height(), 480 / scale());
  createEffect(() => {
    const doc = document();
    setError("");
    setHeight(650);
    if (ready())
      frame.contentWindow?.postMessage(
        { type: "render", html: doc.html, preview: doc.id },
        "*",
      );
  });
  onMount(() => {
    const listen = (event: MessageEvent) => {
      if (event.source !== frame.contentWindow) return;
      if (event.data?.type === "preview-host-ready") {
        setReady(true);
        return;
      }
      const message = Preview.message(event.data, document().id);
      if (message?.type === "size" && Number.isFinite(message.height))
        setHeight(Math.max(100, message.height));
      if (message?.type === "error") setError(message.message);
      if (message?.type === "blocked")
        setError(
          "The preview blocked a network request. Embed data and use supported libraries.",
        );
      if (message?.type === "navigation")
        setError("External navigation is disabled in previews.");
    };
    window.addEventListener("message", listen);
    const observer = new ResizeObserver(() => setWidth(host.clientWidth));
    observer.observe(host);
    onCleanup(() => {
      observer.disconnect();
      window.removeEventListener("message", listen);
    });
  });
  return (
    <div
      class="html-preview"
      ref={host}
      data-preview-error={error() || undefined}
    >
      <div
        class="html-preview-viewport"
        style={
          props.fit ? { height: `${Math.ceil(tall() * scale())}px` } : undefined
        }
      >
        <iframe
          ref={frame}
          title={props.title ?? "HTML preview"}
          src="./preview.html"
          sandbox="allow-scripts allow-downloads"
          referrerpolicy="no-referrer"
          onLoad={() => setReady(true)}
          style={
            props.fit
              ? {
                  width: "1280px",
                  height: `${tall()}px`,
                  transform: `scale(${scale()})`,
                  "transform-origin": "0 0",
                }
              : undefined
          }
        />
      </div>
      {error() && (
        <div class="preview-notice" role="status">
          {error()}
        </div>
      )}
    </div>
  );
}
