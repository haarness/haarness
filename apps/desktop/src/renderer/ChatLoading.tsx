import { Mark } from "./brand";

export function ChatLoading(props: { label: string }) {
  return (
    <div class="chat-loading" role="status" aria-label={props.label}>
      <div class="start-mark" aria-hidden="true">
        <Mark />
      </div>
      <p>{props.label}</p>
    </div>
  );
}
