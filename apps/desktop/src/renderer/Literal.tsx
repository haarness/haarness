import { For } from "solid-js";
import { literalParts } from "./display-text";

/** Text a request carries (a command, a path, a rule), with what is invisible
 * in it shown as quiet codes such as `\u{202E}`, so an approval never hides
 * or reorders what runs. `shows` narrows which characters are codes. */
export function Literal(props: { text: string; shows?: (code: number) => boolean }) {
  return (
    <For each={literalParts(props.text, props.shows)}>
      {(part) => (part.code ? <span class="literal-code">{part.text}</span> : part.text)}
    </For>
  );
}
