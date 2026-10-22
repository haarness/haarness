import test from "node:test";
import assert from "node:assert/strict";
import { createModifiers, createPicker, pickerScript, PICK_WORLD } from "../src/main/browser-picker";
import type { PickedElement, PickEnd } from "../src/browser-pick";

const element: PickedElement = {
  url: "http://localhost:3000/",
  tag: "button#save",
  name: "Save",
  text: "",
  html: '<button id="save">Save</button>',
  selector: "#save",
  width: 80,
  height: 30,
};
/** A page whose isolated world answers the picker's calls, as a loaded page does. */
function fakePage(options: { more?: boolean; nothing?: boolean } = {}) {
  const calls: string[] = [];
  return {
    calls,
    contents: {
      isDestroyed: () => false,
      async executeJavaScriptInIsolatedWorld(world: number, scripts: { code: string }[]) {
        assert.equal(world, PICK_WORLD);
        const call = scripts[0].code.slice(pickerScript.length + 1);
        calls.push(call);
        if (call.includes(".take(")) return options.nothing ? null : { element, more: !!options.more };
        return true;
      },
    },
  };
}
const press = (type: "mouseDown" | "mouseUp" | "mouseMove", x = 50, y = 40) => {
  let prevented = false;
  return {
    event: { preventDefault: () => (prevented = true) } as unknown as Electron.Event,
    input: { type, x, y, button: "left" } as Electron.MouseInputEvent,
    prevented: () => prevented,
  };
};
const settle = () => new Promise((done) => setTimeout(done, 0));

test("the picker script is one expression its isolated world can run again on every call", () => {
  assert.doesNotThrow(() => new Function(pickerScript));
  assert.ok(pickerScript.startsWith("globalThis.__haarnessPicker ??="));
});

test("a press picks instead of reaching the page, and its release and a double click's rest are kept too", async () => {
  const page = fakePage();
  const picker = createPicker(page.contents, { zoom: () => 2, agent: () => false });
  const picked: PickedElement[] = [];
  const ended: PickEnd[] = [];
  await picker.start({ hint: "Click", accent: "#d9874b" }, { picked: (value) => picked.push(value), ended: (reason) => ended.push(reason) });
  assert.match(page.calls[0], /^globalThis\.__haarnessPicker\.start\(\{"hint":"Click","accent":"#d9874b"\}\)$/);
  // The pointer's movement reaches the page, as hover does.
  const move = press("mouseMove");
  assert.equal(picker.mouse(move.event, move.input), false);
  assert.equal(move.prevented(), false);
  const down = press("mouseDown", 100, 60);
  assert.equal(picker.mouse(down.event, down.input), true);
  assert.equal(down.prevented(), true);
  await settle();
  // View pixels become CSS pixels at the page's zoom.
  assert.match(page.calls.at(-2)!, /\.take\(50,30,false\)$/);
  assert.deepEqual(picked, [element]);
  assert.deepEqual(ended, ["picked"]);
  assert.equal(picker.active, false);
  const up = press("mouseUp");
  assert.equal(picker.mouse(up.event, up.input), true);
  const again = press("mouseDown");
  assert.equal(picker.mouse(again.event, again.input), true, "the second press of a double click");
});

test("Shift keeps picking, Escape stops, and nothing over the page picks nothing", async () => {
  const keys = createModifiers();
  const page = fakePage();
  const picker = createPicker(page.contents, { zoom: () => 1, agent: () => false, window: keys.get });
  const ended: PickEnd[] = [];
  let count = 0;
  await picker.start({ hint: "", accent: "" }, { picked: () => count++, ended: (reason) => ended.push(reason) });
  keys.input({ type: "keyDown", shift: true, alt: true });
  const down = press("mouseDown");
  picker.mouse(down.event, down.input);
  await settle();
  assert.equal(count, 1);
  assert.equal(picker.active, true);
  // Option picks exactly what is under the pointer.
  assert.match(page.calls.at(-1)!, /\.take\(50,40,true\)$/);
  keys.input({ type: "keyUp", shift: false, alt: false });
  let prevented = false;
  const escape = { preventDefault: () => (prevented = true) } as unknown as Electron.Event;
  assert.equal(picker.key(escape, { type: "keyDown", key: "Escape", shift: false, alt: false } as Electron.Input), true);
  assert.equal(prevented, true);
  assert.deepEqual(ended, ["cancelled"]);
  // After Escape, nothing is swallowed.
  const later = press("mouseDown");
  assert.equal(picker.mouse(later.event, later.input), false);

  const empty = fakePage({ nothing: true });
  const idle = createPicker(empty.contents, { zoom: () => 1, agent: () => false });
  const reasons: PickEnd[] = [];
  await idle.start({ hint: "", accent: "" }, { picked: () => assert.fail("picked nothing"), ended: (reason) => reasons.push(reason) });
  const blank = press("mouseDown");
  assert.equal(idle.mouse(blank.event, blank.input), true);
  await settle();
  assert.equal(idle.active, true);
  idle.end("navigated");
  assert.deepEqual(reasons, ["navigated"]);
});

test("the agent's own input passes, and its screenshots never show the outline", async () => {
  let dispatching = false;
  const page = fakePage();
  const picker = createPicker(page.contents, { zoom: () => 1, agent: () => dispatching });
  await picker.start({ hint: "", accent: "" }, { picked: () => assert.fail("the agent picked"), ended: () => {} });
  dispatching = true;
  const click = press("mouseDown");
  assert.equal(picker.mouse(click.event, click.input), false);
  assert.equal(click.prevented(), false);
  assert.equal(picker.key({ preventDefault() {} } as unknown as Electron.Event, { type: "keyDown", key: "Escape" } as Electron.Input), false);
  assert.equal(picker.active, true);
  dispatching = false;
  page.calls.length = 0;
  assert.equal(await picker.hidden(async () => "shot"), "shot");
  assert.deepEqual(page.calls, ["globalThis.__haarnessPicker.hide(true)", "globalThis.__haarnessPicker.hide(false)"]);
  // Long after picking ended, screenshots skip the outline's page entirely.
  const quiet = fakePage();
  const unused = createPicker(quiet.contents, { zoom: () => 1, agent: () => false });
  await unused.hidden(async () => undefined);
  assert.deepEqual(quiet.calls, []);
});
