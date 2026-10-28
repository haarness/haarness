import test from "node:test";
import assert from "node:assert/strict";
import { LIFTED_IN_FRAME, createTranscriptPress, listenForPress, type PressPointer } from "../src/renderer/transcript-press";

/** A press tracker and how many times it said the last press ended. */
function tracker() {
  const counted = { ended: 0 };
  const press = createTranscriptPress(() => counted.ended++);
  return { press, counted };
}

const mouse = (button = 0, buttons = 1): PressPointer => ({
  pointerId: 1,
  pointerType: "mouse",
  isPrimary: true,
  button,
  buttons,
});
const touch = (pointerId: number, isPrimary: boolean, buttons = 1): PressPointer => ({
  pointerId,
  pointerType: "touch",
  isPrimary,
  button: 0,
  buttons,
});

test("a main or middle press in the transcript is held until it lifts", () => {
  for (const button of [0, 1]) {
    const { press, counted } = tracker();
    press.press(mouse(button), true);
    assert.equal(press.held(), true, `button ${button}`);
    // Moving with the button down, as a drag that selects does, keeps it.
    press.move(mouse(-1, button === 0 ? 1 : 4));
    assert.equal(press.held(), true);
    assert.equal(counted.ended, 0);
    press.release(mouse(button, 0));
    assert.equal(press.held(), false);
    assert.equal(counted.ended, 1);
    // A second release, or one of a pointer never held, says nothing more.
    press.release(mouse(button, 0));
    press.release(touch(7, false, 0));
    assert.equal(counted.ended, 1);
  }
});

test("a press elsewhere, or with the menu button, is not held", () => {
  const { press, counted } = tracker();
  press.press(mouse(), false);
  assert.equal(press.held(), false);
  press.press(mouse(2, 2), true);
  assert.equal(press.held(), false);
  // Back, forward and a pen's eraser are not presses on the text either.
  for (const button of [3, 4, 5]) press.press({ ...mouse(button, 0), pointerType: "pen" }, true);
  assert.equal(press.held(), false);
  press.release(mouse(0, 0));
  assert.equal(counted.ended, 0);
});

test("with two fingers down the press is held until both lift", () => {
  const { press, counted } = tracker();
  press.press(touch(10, true), true);
  press.press(touch(11, false), true);
  press.release(touch(10, true, 0));
  assert.equal(press.held(), true);
  // The finger still down goes on moving: still held.
  press.move(touch(11, false));
  assert.equal(press.held(), true);
  assert.equal(counted.ended, 0);
  press.release(touch(11, false, 0));
  assert.equal(press.held(), false);
  assert.equal(counted.ended, 1);
});

test("a lost release ends the press: a move with no button down, or a new first touch", () => {
  const { press, counted } = tracker();
  press.press(mouse(), true);
  // The pointer comes back with no button down, as after a drag and drop.
  press.move(mouse(-1, 0));
  assert.equal(press.held(), false);
  assert.equal(counted.ended, 1);
  // A move with no button of another pointer leaves this one held.
  press.press(mouse(), true);
  press.move(touch(3, false, 0));
  assert.equal(press.held(), true);
  press.release(mouse(0, 0));
  assert.equal(counted.ended, 2);

  // A touch whose release never came: a new first touch means no other is down.
  press.press(touch(20, true), true);
  press.press(touch(21, true), false);
  assert.equal(press.held(), false);
  assert.equal(counted.ended, 3);
  // A new first touch in the transcript holds anew, without saying the old one ended.
  press.press(touch(30, true), true);
  press.press(touch(31, true), true);
  assert.equal(press.held(), true);
  assert.equal(counted.ended, 3);
  press.release(touch(30, false, 0));
  assert.equal(press.held(), true);
  press.release(touch(31, true, 0));
  assert.equal(counted.ended, 4);
  // A first press of another kind leaves a held touch alone.
  press.press(touch(40, true), true);
  press.press(mouse(), false);
  assert.equal(press.held(), true);
  // A mouse press outside after one inside means its release was lost.
  press.end();
  press.press(mouse(), true);
  press.press(mouse(), false);
  assert.equal(press.held(), false);
});

test("ending lets go of every press and says so once; clearing forgets them quietly", () => {
  const { press, counted } = tracker();
  press.end();
  assert.equal(counted.ended, 0);
  press.press(touch(1, true), true);
  press.press(touch(2, false), true);
  press.end();
  assert.equal(press.held(), false);
  assert.equal(counted.ended, 1);
  press.press(mouse(), true);
  press.clear();
  assert.equal(press.held(), false);
  assert.equal(counted.ended, 1);
  // Held again afterwards like any press.
  press.press(mouse(), true);
  assert.equal(press.held(), true);
});

class Pointer extends Event {
  constructor(
    type: string,
    readonly pointer: PressPointer,
    readonly inside = true,
  ) {
    super(type);
    Object.assign(this, pointer);
  }
}

function page() {
  const view = new EventTarget();
  const document = Object.assign(new EventTarget(), { hidden: false });
  const { press, counted } = tracker();
  const stop = listenForPress(press, (event) => (event as Pointer).inside, view, document);
  const down = (inside = true, pointer = mouse()) => view.dispatchEvent(new Pointer("pointerdown", pointer, inside));
  return { view, document, press, counted, stop, down };
}

test("the window's pointer events hold and end the press", () => {
  const { view, press, counted, down } = page();
  down(false);
  assert.equal(press.held(), false);
  down();
  assert.equal(press.held(), true);
  view.dispatchEvent(new Pointer("pointermove", mouse(-1, 1)));
  assert.equal(press.held(), true);
  view.dispatchEvent(new Pointer("pointerup", mouse(0, 0)));
  assert.equal(press.held(), false);
  down();
  view.dispatchEvent(new Pointer("pointercancel", mouse(0, 0)));
  assert.equal(press.held(), false);
  down();
  view.dispatchEvent(new Pointer("pointermove", mouse(-1, 0)));
  assert.equal(press.held(), false);
  assert.equal(counted.ended, 3);
});

test("a menu, a button let go over an app's frame, the window losing focus or the page hiding ends a press whose release may never come", () => {
  const { view, document, press, counted, down } = page();
  down();
  view.dispatchEvent(new Event("contextmenu"));
  assert.equal(press.held(), false);
  down();
  view.dispatchEvent(new Event(LIFTED_IN_FRAME));
  assert.equal(press.held(), false);
  down();
  view.dispatchEvent(new Event("blur"));
  assert.equal(press.held(), false);
  down();
  // Shown again, or still shown: the press goes on.
  document.dispatchEvent(new Event("visibilitychange"));
  assert.equal(press.held(), true);
  document.hidden = true;
  document.dispatchEvent(new Event("visibilitychange"));
  assert.equal(press.held(), false);
  assert.equal(counted.ended, 4);
});

test("a control losing focus is not the window losing it, and stopping stops listening", () => {
  const { view, press, counted, stop, down } = page();
  down();
  // A blur the window sees on its way to a control in it.
  const relayed = new Event("blur");
  Object.defineProperty(relayed, "target", { value: new EventTarget() });
  view.dispatchEvent(relayed);
  assert.equal(press.held(), true);
  stop();
  view.dispatchEvent(new Pointer("pointerup", mouse(0, 0)));
  view.dispatchEvent(new Event("contextmenu"));
  assert.equal(press.held(), true);
  assert.equal(counted.ended, 0);
});
