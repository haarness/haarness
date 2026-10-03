import {
  createEffect,
  createSignal,
  createUniqueId,
  For,
  on,
  onCleanup,
  Show,
} from "solid-js";
import { Portal } from "solid-js/web";
import type { JSX } from "solid-js";
import { Icon, type IconName } from "./ui";

export interface MenuAction {
  label: string;
  /** One quiet line under the label, for choices that need to be explained. */
  detail?: string;
  icon?: IconName;
  disabled?: boolean;
  /** Marks the live choice in a group of alternatives. */
  checked?: boolean;
  /** Nested alternatives. An item either runs or opens these, never both. */
  submenu?: MenuAction[];
  /** Starts a new group: a hairline is drawn above this item. */
  divider?: boolean;
  run?: () => void;
}

/** The hairline between two groups of choices; keyboard navigation skips it. */
const Divider = (props: { when?: boolean }) => (
  <Show when={props.when}>
    <div class="menu-divider" role="separator" />
  </Show>
);

// Shared anchored menu: direct actions, keyboard navigation and viewport-aware
// placement. Portals stay inside native dialogs when opened from Settings.
export function MenuButton(props: {
  label: string;
  icon?: IconName;
  text?: string;
  /** Custom trigger content, for triggers that are not icon + label. */
  content?: JSX.Element;
  class?: string;
  title?: string;
  items: MenuAction[];
  /** A menu that replaces its trigger's value is at least as wide as it. */
  matchWidth?: boolean;
  onKeyDown?: (
    event: KeyboardEvent & { currentTarget: HTMLButtonElement },
  ) => void;
}) {
  const [open, setOpen] = createSignal(false);
  /** The submenu currently unfolded, keyed by its parent item's label. */
  const [branch, setBranch] = createSignal("");
  const [position, setPosition] = createSignal({ left: 0, top: 0, width: 0 });
  const id = createUniqueId();
  let handle!: HTMLButtonElement;
  let menu!: HTMLDivElement;
  const close = (focus = false) => {
    setOpen(false);
    setBranch("");
    if (focus) handle.focus({ preventScroll: true });
  };
  createEffect(
    on(open, (visible) => {
      if (!visible) return;
      const outside = (event: Event) => {
        if (
          !menu?.contains(event.target as Node) &&
          !handle.contains(event.target as Node)
        )
          close();
      };
      const positionMenu = () => {
        const anchor = handle.getBoundingClientRect();
        const box = menu.getBoundingClientRect();
        setPosition({
          width: props.matchWidth ? anchor.width : 0,
          left: Math.max(
            8,
            Math.min(anchor.left, window.innerWidth - box.width - 8),
          ),
          top: Math.max(
            8,
            anchor.bottom + box.height + 6 <= window.innerHeight - 8
              ? anchor.bottom + 6
              : anchor.top - box.height - 6,
          ),
        });
      };
      const scroll = (event: Event) => {
        // Only the anchor's ancestors affect placement. Streaming or resizing
        // another pane must not dismiss a menu the user is interacting with.
        if (
          event.target === document ||
          (event.target instanceof Element && event.target.contains(handle))
        )
          positionMenu();
      };
      queueMicrotask(() => {
        if (!open()) return;
        positionMenu();
        (
          menu.querySelector<HTMLButtonElement>(
            '[role="menuitem"]:not(:disabled), [role="menuitemradio"]:not(:disabled)',
          ) ?? menu
        ).focus({ preventScroll: true });
      });
      document.addEventListener("pointerdown", outside);
      document.addEventListener("focusin", outside);
      document.addEventListener("scroll", scroll, true);
      document.addEventListener("dragstart", outside);
      window.addEventListener("resize", positionMenu);
      onCleanup(() => {
        document.removeEventListener("pointerdown", outside);
        document.removeEventListener("focusin", outside);
        document.removeEventListener("scroll", scroll, true);
        document.removeEventListener("dragstart", outside);
        window.removeEventListener("resize", positionMenu);
      });
    }),
  );
  return (
    <>
      <button
        ref={handle}
        type="button"
        class={
          props.class ?? (props.text || props.content ? "" : "icon-button")
        }
        aria-label={props.label}
        aria-haspopup="menu"
        aria-expanded={open()}
        aria-controls={open() ? id : undefined}
        title={props.title ?? props.label}
        onClick={(event) => {
          event.stopPropagation();
          setOpen((value) => !value);
        }}
        onKeyDown={(event) => {
          props.onKeyDown?.(event);
          if (!event.defaultPrevented && event.key === "ArrowDown") {
            event.preventDefault();
            event.stopPropagation();
            setOpen(true);
          }
        }}
      >
        <Show when={props.icon}>
          <Icon name={props.icon!} size="md" />
        </Show>
        {props.content}
        {props.text}
      </button>
      <Show when={open()}>
        <Portal
          mount={handle.closest('dialog, [role="dialog"]') ?? document.body}
        >
          <div
            ref={menu}
            id={id}
            class="action-menu popover"
            role="menu"
            aria-label={props.label}
            tabIndex={-1}
            style={{
              left: position().left + "px",
              top: position().top + "px",
              ...(position().width ? { "min-width": position().width + "px" } : {}),
            }}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                close(true);
                return;
              }
              if (event.key === "Tab") {
                close(true);
                return;
              }
              if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key))
                return;
              event.preventDefault();
              event.stopPropagation();
              const items = Array.from(
                menu.querySelectorAll<HTMLButtonElement>(
                  '[role="menuitem"]:not(:disabled), [role="menuitemradio"]:not(:disabled)',
                ),
              );
              const current = items.indexOf(
                document.activeElement as HTMLButtonElement,
              );
              const next =
                event.key === "Home"
                  ? 0
                  : event.key === "End"
                    ? items.length - 1
                    : (current +
                        (event.key === "ArrowDown" ? 1 : -1) +
                        items.length) %
                      items.length;
              items[next]?.focus();
            }}
          >
            <For each={props.items}>
              {(item) => (
                <Show
                  when={item.submenu?.length}
                  fallback={
                    <>
                      <Divider when={item.divider} />
                      <MenuItem
                        item={item}
                        /* Moving to a sibling folds an open branch, as native
                           menus do. */
                        onPointerEnter={() => setBranch("")}
                        onRun={() => {
                          close(true);
                          item.run?.();
                          queueMicrotask(() => {
                            if (
                              handle.isConnected &&
                              (document.activeElement === document.body ||
                                document.activeElement === handle)
                            ) {
                              handle.focus({ preventScroll: true });
                              handle.scrollIntoView({
                                block: "nearest",
                                inline: "nearest",
                              });
                            }
                          });
                        }}
                      />
                    </>
                  }
                >
                  <Divider when={item.divider} />
                  <div
                    class="menu-branch"
                    onPointerEnter={() => setBranch(item.label)}
                  >
                    <button
                      role="menuitem"
                      aria-haspopup="menu"
                      aria-expanded={branch() === item.label}
                      aria-label={item.label}
                      disabled={item.disabled}
                      onClick={(event) => {
                        // Hovering already unfolded the branch, so a click
                        // confirms it rather than folding it back up.
                        event.stopPropagation();
                        setBranch(item.label);
                        queueMicrotask(() =>
                          menu
                            .querySelector<HTMLButtonElement>(
                              '.menu-branch-panel [role="menuitem"]:not(:disabled), .menu-branch-panel [role="menuitemradio"]:not(:disabled)',
                            )
                            ?.focus(),
                        );
                      }}
                      onKeyDown={(event) => {
                        if (event.key === "ArrowRight" || event.key === "Enter") {
                          event.preventDefault();
                          event.stopPropagation();
                          setBranch(item.label);
                          queueMicrotask(() =>
                            menu
                              .querySelector<HTMLButtonElement>(
                                '.menu-branch-panel [role="menuitem"]:not(:disabled), .menu-branch-panel [role="menuitemradio"]:not(:disabled)',
                              )
                              ?.focus(),
                          );
                        }
                      }}
                    >
                      <Show when={item.icon}>
                        <Icon name={item.icon!} size="sm" />
                      </Show>
                      <span class="menu-branch-label">{item.label}</span>
                      <Icon name="chevron" size="sm" />
                    </button>
                    <Show when={branch() === item.label}>
                      <div
                        class="action-menu popover menu-branch-panel"
                        role="menu"
                        aria-label={item.label}
                        onKeyDown={(event) => {
                          if (event.key !== "ArrowLeft") return;
                          event.preventDefault();
                          event.stopPropagation();
                          setBranch("");
                          menu
                            .querySelector<HTMLButtonElement>(
                              '[role="menuitem"][aria-haspopup="menu"]',
                            )
                            ?.focus();
                        }}
                      >
                        <For each={item.submenu}>
                          {(child) => (
                            <>
                              <Divider when={child.divider} />
                              <MenuItem
                                item={child}
                                onRun={() => {
                                  close(true);
                                  child.run?.();
                                }}
                              />
                            </>
                          )}
                        </For>
                      </div>
                    </Show>
                  </div>
                </Show>
              )}
            </For>
          </div>
        </Portal>
      </Show>
    </>
  );
}

/** A leaf choice. `checked` turns it into a radio within its group. */
function MenuItem(props: {
  item: MenuAction;
  onRun: () => void;
  onPointerEnter?: () => void;
}) {
  return (
    <button
      role={props.item.checked === undefined ? "menuitem" : "menuitemradio"}
      aria-checked={props.item.checked}
      onPointerEnter={props.onPointerEnter}
      /* The action names the item; any explanation is a description, so the
         accessible name stays the verb. */
      aria-label={props.item.label}
      aria-description={props.item.detail}
      disabled={props.item.disabled}
      onClick={(event) => {
        event.stopPropagation();
        props.onRun();
      }}
    >
      <Show when={props.item.checked !== undefined}>
        <span class="menu-check" aria-hidden="true">
          <Show when={props.item.checked}>
            <Icon name="check" size="sm" />
          </Show>
        </span>
      </Show>
      <Show when={props.item.icon}>
        <Icon name={props.item.icon!} size="sm" />
      </Show>
      <Show when={props.item.detail} fallback={props.item.label}>
        <span class="menu-item-label">
          {props.item.label}
          <small>{props.item.detail}</small>
        </span>
      </Show>
    </button>
  );
}
