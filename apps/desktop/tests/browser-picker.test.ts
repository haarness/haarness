import test from "node:test";
import assert from "node:assert/strict";
import { agentEvents, createModifiers, createPicker, pickerScript, PICK_WORLD } from "../src/main/browser-picker";
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
function fakePage(options: { more?: boolean; nothing?: boolean; startDelay?: Promise<void> } = {}) {
  const calls: string[] = [];
  return {
    calls,
    contents: {
      isDestroyed: () => false,
      async executeJavaScriptInIsolatedWorld(world: number, scripts: { code: string }[]) {
        assert.equal(world, PICK_WORLD);
        const call = scripts[0].code.slice(pickerScript.length + 1);
        calls.push(call);
        if (call.includes(".start(")) await options.startDelay;
        if (call.includes(".take(")) return options.nothing ? null : { element, more: !!options.more };
        return true;
      },
    },
  };
}
const mouse = (type: "mouseDown" | "mouseUp" | "mouseMove" | "contextMenu", x = 50, y = 40, button = "left") => {
  let prevented = false;
  return {
    event: { preventDefault: () => (prevented = true) } as unknown as Electron.Event,
    input: { type, x, y, button } as Electron.MouseInputEvent,
    prevented: () => prevented,
  };
};
const key = (type: "keyDown" | "keyUp", name: string, extra: Partial<Electron.Input> = {}) => {
  let prevented = false;
  return {
    event: { preventDefault: () => (prevented = true) } as unknown as Electron.Event,
    input: { type, key: name, code: "", shift: false, alt: false, meta: false, control: false, ...extra } as Electron.Input,
    prevented: () => prevented,
  };
};
const settle = () => new Promise((done) => setTimeout(done, 0));
const quiet = { picked: () => {}, ended: () => {} };

test("the picker script is one expression its isolated world can run again on every call", () => {
  assert.doesNotThrow(() => new Function(pickerScript));
  assert.ok(pickerScript.startsWith("globalThis.__haarnessPicker ??="));
});

test("an agent command says which presses and keys it will make, in view pixels", () => {
  assert.deepEqual(agentEvents("Input.dispatchMouseEvent", { type: "mousePressed", x: 10, y: 20, button: "left" }, 2, { x: 900, y: 200 }), [
    { kind: "mouse", type: "mouseDown", x: 20, y: 40, screenX: 920, screenY: 240, button: "left" },
  ]);
  assert.deepEqual(
    agentEvents("Input.dispatchMouseEvent", { type: "mousePressed", x: 1, y: 2, button: "right" }, 1).map((event) => event.type),
    ["mouseDown", "contextMenu"],
  );
  assert.deepEqual(agentEvents("Input.dispatchKeyEvent", { type: "rawKeyDown", key: "a", code: "KeyA" }, 1), [
    { kind: "key", type: "keyDown", key: "a", code: "KeyA" },
  ]);
  assert.deepEqual(agentEvents("Input.dispatchKeyEvent", { type: "char", text: "a" }, 1), []);
  assert.deepEqual(agentEvents("Input.insertText", { text: "typed" }, 1), []);
  assert.deepEqual(agentEvents("Input.dispatchDragEvent", { type: "drop", x: 1, y: 1 }, 1), []);
});

test("a press picks instead of reaching the page, and its release and a double click's rest are kept too", async () => {
  const page = fakePage();
  const picker = createPicker(page.contents, { zoom: () => 2 });
  const picked: PickedElement[] = [];
  const ended: PickEnd[] = [];
  assert.equal(
    await picker.start({ hint: "Click", accent: "#d9874b" }, { picked: (value) => picked.push(value), ended: (reason) => ended.push(reason) }),
    true,
  );
  assert.match(page.calls[0], /^globalThis\.__haarnessPicker\.start\(\{"hint":"Click","accent":"#d9874b"\}\)$/);
  // The pointer's movement reaches the page, as hover does.
  const move = mouse("mouseMove");
  assert.equal(picker.mouse(move.event, move.input), false);
  assert.equal(move.prevented(), false);
  const down = mouse("mouseDown", 100, 60);
  assert.equal(picker.mouse(down.event, down.input), true);
  assert.equal(down.prevented(), true);
  await settle();
  // View pixels become CSS pixels at the page's zoom.
  assert.match(page.calls.at(-2)!, /\.take\(50,30,false\)$/);
  assert.deepEqual(picked, [element]);
  assert.deepEqual(ended, ["picked"]);
  assert.equal(picker.active, false);
  const up = mouse("mouseUp");
  assert.equal(picker.mouse(up.event, up.input), true);
  const again = mouse("mouseDown");
  assert.equal(picker.mouse(again.event, again.input), true, "the second press of a double click");
});

test("only the presses and keys an agent command makes pass while the person picks", async () => {
  const page = fakePage();
  const picker = createPicker(page.contents, { zoom: () => 1 });
  let picks = 0;
  await picker.start({ hint: "", accent: "" }, { picked: () => picks++, ended: () => {} });
  let release!: () => void;
  const typing = picker.dispatch("Input.dispatchKeyEvent", { type: "keyDown", key: "a" }, () => new Promise<void>((done) => (release = done)));
  let pressing!: () => void;
  const clicking = picker.dispatch(
    "Input.dispatchMouseEvent",
    { type: "mousePressed", x: 200, y: 120, button: "left" },
    () => new Promise<void>((done) => (pressing = done)),
  );
  await settle();
  // The agent's own press and key reach the page.
  const agentPress = mouse("mouseDown", 200, 120);
  assert.equal(picker.mouse(agentPress.event, agentPress.input), false);
  assert.equal(agentPress.prevented(), false);
  const agentKey = key("keyDown", "a");
  assert.equal(picker.key(agentKey.event, agentKey.input), false);
  // The person's press elsewhere at the same moment is theirs: it picks.
  const personPress = mouse("mouseDown", 30, 30);
  assert.equal(picker.mouse(personPress.event, personPress.input), true);
  assert.equal(personPress.prevented(), true);
  await settle();
  assert.equal(picks, 1);
  release();
  pressing();
  await Promise.all([typing, clicking]);
  // Once the agent's command is done, the same press is the person's.
  const late = mouse("mouseDown", 200, 120);
  assert.equal(picker.mouse(late.event, late.input), true);
});

test("while picking the page hears none of the person's keys; Escape stops, even while the agent types", async () => {
  const page = fakePage();
  const picker = createPicker(page.contents, { zoom: () => 1 });
  const ended: PickEnd[] = [];
  await picker.start({ hint: "", accent: "" }, { picked: () => {}, ended: (reason) => ended.push(reason) });
  for (const name of [" ", "Enter", "d", "Tab"]) {
    const press = key("keyDown", name);
    assert.equal(picker.key(press.event, press.input), true, name);
    assert.equal(press.prevented(), true, name);
  }
  // The app's own shortcuts still work (⇧⌘C stops picking from the page).
  const chord = key("keyDown", "c", { meta: true, shift: true });
  assert.equal(picker.key(chord.event, chord.input), false);
  let release!: () => void;
  const typing = picker.dispatch("Input.dispatchKeyEvent", { type: "keyDown", key: "x" }, () => new Promise<void>((done) => (release = done)));
  await settle();
  const escape = key("keyDown", "Escape");
  assert.equal(picker.key(escape.event, escape.input), true);
  assert.equal(escape.prevented(), true);
  assert.deepEqual(ended, ["cancelled"]);
  // Its release is kept from the page too; later keys are the page's again.
  const up = key("keyUp", "Escape");
  assert.equal(picker.key(up.event, up.input), true);
  const after = key("keyDown", "d");
  assert.equal(picker.key(after.event, after.input), false);
  release();
  await typing;
});

test("an agent's click lifts the frames' pointer rule once, until a moment after its last press", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const page = fakePage();
  const picker = createPicker(page.contents, { zoom: () => 1 });
  // Not picking: nothing to lift.
  await picker.dispatch("Input.dispatchMouseEvent", { type: "mousePressed", x: 1, y: 1, button: "left" }, async () => {});
  assert.deepEqual(page.calls, []);
  await picker.start({ hint: "", accent: "" }, quiet);
  page.calls.length = 0;
  for (const type of ["mouseMoved", "mousePressed", "mouseReleased"])
    await picker.dispatch("Input.dispatchMouseEvent", { type, x: 1, y: 1, button: "left" }, async () => {
      assert.deepEqual(page.calls, ["globalThis.__haarnessPicker.frames(false)"]);
    });
  t.mock.timers.tick(249);
  assert.deepEqual(page.calls, ["globalThis.__haarnessPicker.frames(false)"]);
  t.mock.timers.tick(1);
  assert.deepEqual(page.calls, ["globalThis.__haarnessPicker.frames(false)", "globalThis.__haarnessPicker.frames(true)"]);
  // Keys go to the focused frame; they lift nothing.
  page.calls.length = 0;
  await picker.dispatch("Input.dispatchKeyEvent", { type: "keyDown", key: "a" }, async () => {});
  assert.deepEqual(page.calls, []);
});

test("a press that lands in a frame of another process, at the frame's own coordinates, is still the agent's", async () => {
  const page = fakePage();
  const picker = createPicker(page.contents, { zoom: () => 1, origin: () => ({ x: 986, y: 202 }) });
  await picker.start({ hint: "", accent: "" }, quiet);
  let release!: () => void;
  const releasing = picker.dispatch(
    "Input.dispatchMouseEvent",
    { type: "mouseReleased", x: 88.6, y: 432.6, button: "left" },
    () => new Promise<void>((done) => (release = done)),
  );
  await settle();
  const framed = { type: "mouseUp", x: 55.6, y: 18.7, globalX: 1074.6, globalY: 634.6, button: "left" } as Electron.MouseInputEvent;
  let prevented = false;
  assert.equal(picker.mouse({ preventDefault: () => (prevented = true) } as unknown as Electron.Event, framed), false);
  assert.equal(prevented, false);
  // The person's press at the frame's same spot but elsewhere on the screen is theirs.
  const person = { ...framed, globalX: 400, globalY: 300 };
  assert.equal(picker.mouse({ preventDefault() {} } as unknown as Electron.Event, person), true);
  release();
  await releasing;
});

test("Shift keeps picking, a released key is forgotten when the keyboard leaves, and nothing over the page picks nothing", async () => {
  const keys = createModifiers();
  const page = fakePage();
  const picker = createPicker(page.contents, { zoom: () => 1, window: keys.get });
  const ended: PickEnd[] = [];
  let count = 0;
  await picker.start({ hint: "", accent: "" }, { picked: () => count++, ended: (reason) => ended.push(reason) });
  keys.input({ type: "keyDown", shift: true, alt: true });
  const down = mouse("mouseDown");
  picker.mouse(down.event, down.input);
  await settle();
  assert.equal(count, 1);
  assert.equal(picker.active, true);
  // Option picks exactly what is under the pointer.
  assert.match(page.calls.at(-1)!, /\.take\(50,40,true\)$/);
  // Shift released in another app never reports; the window's blur forgets it.
  keys.reset();
  assert.deepEqual(keys.get(), { shift: false, alt: false });
  const next = mouse("mouseDown");
  picker.mouse(next.event, next.input);
  await settle();
  assert.equal(count, 2);
  assert.deepEqual(ended, ["picked"]);

  const empty = fakePage({ nothing: true });
  const idle = createPicker(empty.contents, { zoom: () => 1 });
  const reasons: PickEnd[] = [];
  await idle.start({ hint: "", accent: "" }, { picked: () => assert.fail("picked nothing"), ended: (reason) => reasons.push(reason) });
  const blank = mouse("mouseDown");
  assert.equal(idle.mouse(blank.event, blank.input), true);
  await settle();
  assert.equal(idle.active, true);
  idle.end("navigated");
  assert.deepEqual(reasons, ["navigated"]);
});

test("picking that ended while the page got ready reports it did not start", async () => {
  let ready!: () => void;
  const page = fakePage({ startDelay: new Promise((done) => (ready = done)) });
  const picker = createPicker(page.contents, { zoom: () => 1 });
  const starting = picker.start({ hint: "", accent: "" }, quiet);
  picker.end("navigated");
  ready();
  assert.equal(await starting, false);
  assert.equal(picker.active, false);
});

test("the agent's screenshots never show the outline", async () => {
  const page = fakePage();
  const picker = createPicker(page.contents, { zoom: () => 1 });
  await picker.start({ hint: "", accent: "" }, quiet);
  page.calls.length = 0;
  assert.equal(await picker.hidden(async () => "shot"), "shot");
  assert.deepEqual(page.calls, ["globalThis.__haarnessPicker.hide(true)", "globalThis.__haarnessPicker.hide(false)"]);
  // Long after picking ended, screenshots skip the outline's page entirely.
  const quietPage = fakePage();
  const unused = createPicker(quietPage.contents, { zoom: () => 1 });
  await unused.hidden(async () => undefined);
  assert.deepEqual(quietPage.calls, []);
});
