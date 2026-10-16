import test from "node:test";
import assert from "node:assert/strict";
import { createCycle, step, type CyclePanel } from "../src/renderer/tab-cycle";

/** An element with only what routing reads: its marker and its parent. */
class Fake {
  constructor(
    readonly marker: string | null,
    readonly parentElement: Fake | null = null,
  ) {}
  getAttribute(name: string) {
    return name === "data-cycle-panel" ? this.marker : null;
  }
  closest(selector: string): Fake | null {
    assert.equal(selector, "[data-cycle-panel]");
    return this.marker !== null ? this : (this.parentElement?.closest(selector) ?? null);
  }
}
const asElement = (fake: Fake) => fake as unknown as Element;

function panel(id: string, tabs: string[], active?: string) {
  const selected: string[] = [];
  const value: CyclePanel & { selected: string[]; current?: string } = {
    id,
    selected,
    current: active ?? tabs[0],
    tabs: () => tabs,
    active: () => value.current,
    select: (tab) => {
      selected.push(tab);
      value.current = tab;
    },
  };
  return value;
}

test("a tab step wraps past either end and starts from an end when the shown tab is unknown", () => {
  const tabs = ["a", "b", "c"];
  assert.equal(step(tabs, "a", 1), "b");
  assert.equal(step(tabs, "c", 1), "a");
  assert.equal(step(tabs, "a", -1), "c");
  assert.equal(step(tabs, "x", 1), "a");
  assert.equal(step(tabs, undefined, -1), "c");
  assert.equal(step(["only"], "only", 1), "only");
  assert.equal(step([], "a", 1), undefined);
});

test("Ctrl+Tab steps the tabs of the innermost panel with the keyboard, and only there", () => {
  const cycle = createCycle(() => {});
  const dock = panel("dock", ["files", "diff", "browser"], "files");
  const files = panel("files", ["a.ts", "b.ts"]);
  const terminal = panel("terminal", ["one", "two", "three"]);
  const chats = panel("chats", ["chat-1", "chat-2"]);
  for (const value of [dock, files, terminal, chats]) cycle.register(value);
  const body = new Fake(null);
  const side = new Fake("dock", body);
  const explorer = new Fake("files", side);
  const editor = new Fake(null, new Fake(null, explorer));
  const review = new Fake(null, side);
  const shell = new Fake(null, new Fake("terminal", body));
  const composer = new Fake(null, new Fake("chats", body));
  const sidebar = new Fake(null, body);

  // The open files, from an editor inside the Files panel.
  assert.equal(cycle.step(asElement(editor), 1), true);
  assert.deepEqual(files.selected, ["b.ts"]);
  // The side panel's own tabs from Review, which has none of its own.
  assert.equal(cycle.step(asElement(review), -1), true);
  assert.deepEqual(dock.selected, ["browser"]);
  // Three presses in a row in the terminal walk its tabs and wrap.
  for (let press = 0; press < 3; press++) cycle.step(asElement(shell), 1);
  assert.deepEqual(terminal.selected, ["two", "three", "one"]);
  // The chat tabs from the message box.
  cycle.step(asElement(composer), 1);
  assert.deepEqual(chats.selected, ["chat-2"]);
  // The sidebar is no panel: the key is left to it.
  assert.equal(cycle.step(asElement(sidebar), 1), false);
  assert.equal(cycle.step(null, 1), false);
});

test("a panel with no tabs passes the step out, and a single tab takes it and selects nothing", () => {
  const cycle = createCycle(() => {});
  const dock = panel("dock", ["files", "browser"], "files");
  const files = panel("files", []);
  const only = panel("terminal", ["one"]);
  cycle.register(dock);
  cycle.register(files);
  const unregister = cycle.register(only);
  const side = new Fake("dock");
  const tree = new Fake(null, new Fake("files", side));
  // No open file: the side panel's tabs step instead.
  cycle.step(asElement(tree), 1);
  assert.deepEqual(dock.selected, ["browser"]);
  const shell = new Fake(null, new Fake("terminal"));
  assert.equal(cycle.step(asElement(shell), 1), true);
  assert.deepEqual(only.selected, []);
  // An unregistered panel takes no step.
  unregister();
  assert.equal(cycle.step(asElement(shell), 1), false);
});
