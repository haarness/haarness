import { For } from "solid-js";
import { literalParts } from "./display-text";

/** Text a request carries (a command, a path, a rule), with what is invisible
 * in it shown as quiet codes such as `\u{202E}`, so an approval never hides
 * or reorders what runs. */
export function Literal(props: { text: string }) {
  return (
    <For each={literalParts(props.text)}>
      {(part) => (part.code ? <span class="literal-code">{part.text}</span> : part.text)}
    </For>
  );
}
