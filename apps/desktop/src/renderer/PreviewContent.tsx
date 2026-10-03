import { Show } from "solid-js";
import type { FileContent } from "../shared";
import { Preview } from "./preview";
import { HtmlPreview } from "./HtmlPreview";
import { Markdown } from "./Markdown";
export function PreviewContent(props: { file: FileContent; fit?: boolean }) {
  const kind = () => Preview.kind(props.file.path);
  return (
    <Show
      when={kind() === "html" || kind() === "svg"}
      fallback={
        <Show
          when={kind() === "markdown"}
          fallback={
            <div class="media-preview">
              <img
                alt={props.file.path}
                src={`data:${props.file.mime};base64,${props.file.content}`}
              />
            </div>
          }
        >
          <div class="document-preview">
            <Markdown text={props.file.content} />
          </div>
        </Show>
      }
    >
      <HtmlPreview
        html={props.file.content}
        title={props.file.path}
        fit={props.fit}
      />
    </Show>
  );
}
