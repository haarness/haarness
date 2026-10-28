import { For } from "solid-js";
import { spelledOutParts, spelledTextParts } from "./display-text";

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

/** Text `spelledOutText` already wrote (a row's target), with its codes set
 * apart as `SpelledOut` sets them and no isolate characters left in the page,
 * so a copy of it is the text as shown. */
export function SpelledText(props: { text: string }) {
  return (
    <For each={spelledTextParts(props.text)}>
      {(part) => (part.code ? <span class="spelled-code">{part.text}</span> : part.text)}
    </For>
  );
}
