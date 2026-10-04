import { t } from "./i18n";
import {
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  createUniqueId,
  on,
  onCleanup,
} from "solid-js";
import type { TodoEntry } from "@haarness/sdk";
import { Button, Icon } from "./ui";
import { useApp } from "./state";

/** One live checklist beside the composer, independent of transcript scrolling. */
export function PlanDock(props: {
  id: string;
  items: TodoEntry[];
  scope: string;
  running: boolean;
  onDismiss: () => void;
}) {
  const app = useApp();
  const id = createUniqueId();
  const identity = createMemo(() => JSON.stringify([props.scope, props.id]));
  const [record, { mutate }] = createResource(identity, async (identity) => {
    // Workspace paths can exceed the desktop store's key-length limit.
    const digest = await crypto.subtle.digest(
      "SHA-256",
      new TextEncoder().encode(identity),
    );
    const key =
      "plan-dismissed:v1:" +
      Array.from(new Uint8Array(digest), (byte) =>
        byte.toString(16).padStart(2, "0"),
      ).join("");
    return {
      key,
      dismissed: (await app.run(() => window.haarness.storage(key))) === true,
    };
  });
  let list!: HTMLUListElement;
  const [expanded, setExpanded] = createSignal<boolean>();
  const done = createMemo(
    () => props.items.filter((item) => item.status === "completed").length,
  );
  const cancelled = createMemo(
    () => props.items.filter((item) => item.status === "cancelled").length,
  );
  const settled = () => done() + cancelled() === props.items.length;
  const visible = createMemo(
    () => !settled() && !record.loading && !!record() && !record()!.dismissed,
  );
  const open = createMemo(() => expanded() ?? (props.running && !settled()));
  const progress = createMemo(() =>
    props.items.findIndex((item) => item.status === "in_progress"),
  );
  const current = createMemo(
    () =>
      props.items.find((item) => item.status === "in_progress") ??
      props.items.find((item) => item.status === "pending"),
  );
  const summary = () =>
    `${done()} of ${props.items.length} completed${cancelled() ? ` · ${cancelled()} cancelled` : ""}`;
  createEffect(on(identity, () => setExpanded(undefined)));
  createEffect(
    on(settled, (value) => {
      if (value) setExpanded(undefined);
    }),
  );
  createEffect(
    on([visible, open, progress], () => {
      if (!visible() || !open()) return;
      const frame = requestAnimationFrame(() => {
        if (!list?.isConnected) return;
        const item = list.querySelector<HTMLElement>(
          '[data-status="in_progress"]',
        );
        if (!item) return;
        const bounds = list.getBoundingClientRect();
        const row = item.getBoundingClientRect();
        if (row.top < bounds.top) list.scrollTop += row.top - bounds.top;
        else if (row.bottom > bounds.bottom)
          list.scrollTop += row.bottom - bounds.bottom;
      });
      onCleanup(() => cancelAnimationFrame(frame));
    }),
  );
  return (
    <Show when={visible()}>
      <section class="plan-dock" aria-label={t("Plan")}>
        <div class="plan-header">
          <button
            class="plan-toggle"
            aria-label={open() ? t("Collapse plan") : t("Expand plan")}
            aria-expanded={open()}
            aria-controls={id}
            onClick={() => setExpanded(!open())}
          >
            <Icon name="todo" size="sm" />
            <span class="plan-title">{t("Plan")}</span>
            <span class="plan-count">{summary()}</span>
            <span
              class="plan-preview"
              title={!open() ? current()?.content : undefined}
            >
              {!open() ? current()?.content : ""}
            </span>
            <Icon name="down" size="xs" class="plan-chevron" />
          </button>
          <Button
            class="plan-dismiss"
            icon="close"
            label={t("Dismiss plan")}
            hint={t("Hide this checklist until a new plan starts")}
            onClick={() => {
              const value = record()!;
              mutate({ ...value, dismissed: true });
              props.onDismiss();
              void app.run(() => window.haarness.storage(value.key, true));
            }}
          />
        </div>
        <span class="sr-only" role="status">
          {summary()}
        </span>
        <ul ref={list} id={id} class="plan-list" hidden={!open()}>
          <For each={props.items}>
            {(item) => (
              <li data-status={item.status}>
                <span class="plan-check" aria-hidden="true">
                  <Show when={item.status === "completed"}>
                    <Icon name="check" size="xs" />
                  </Show>
                </span>
                <span class="sr-only">
                  {item.status === "in_progress"
                    ? t("In progress")
                    : item.status === "completed"
                      ? t("Completed")
                      : item.status === "cancelled"
                        ? t("Cancelled")
                        : t("Pending")}
                  :{" "}
                </span>
                <span class="plan-content">{item.content}</span>
              </li>
            )}
          </For>
        </ul>
      </section>
    </Show>
  );
}
