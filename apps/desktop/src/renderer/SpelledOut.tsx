import { For } from "solid-js";
import { spelledOutParts } from "./display-text";

/** Text a request carries (a command, a path, a rule), with what is invisible
 * in it shown as quiet codes such as `\u{202E}`, so an approval never hides
 * or reorders what runs. `shows` narrows which characters are codes. */
export function SpelledOut(props: { text: string; shows?: (code: number) => boolean }) {
  return (
    <For each={spelledOutParts(props.text, props.shows)}>
      {(part) => (part.code ? <span class="spelled-code">{part.text}</span> : part.text)}
    </For>
  );
}
