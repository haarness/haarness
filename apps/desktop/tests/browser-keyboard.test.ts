import { test } from "node:test";
import assert from "node:assert/strict";
import { createKeyboardHandoff, giveKeyboard, returnKeyboard } from "../src/main/browser-keyboard";

/** A window and a page, with what the window's renderer answers about its focused element. */
function setup(state: { window?: boolean; page?: boolean; composer?: boolean; blurWhileAsking?: boolean } = {}) {
  const calls: string[] = [];
  let windowFocused = state.window ?? true;
  const win = {
    isDestroyed: () => false,
    isFocused: () => windowFocused,
    webContents: {
      focus: () => void calls.push("focus"),
      executeJavaScript: async (code: string) => {
        calls.push("ask");
        assert.match(code, /\[data-composer-input\]/);
        if (state.blurWhileAsking) windowFocused = false;
        return state.composer ?? true;
      },
    },
  };
  const page = { isDestroyed: () => false, isFocused: () => state.page ?? true };
  return { calls, run: () => returnKeyboard(win as never, page as never) };
}

test("a page put away while it has the keyboard gives it back to the message box", async () => {
  const { calls, run } = setup();
  assert.equal(await run(), true);
  assert.deepEqual(calls, ["ask", "focus"]);
});

test("the app never takes the keyboard while another application has it", async () => {
  const away = setup({ window: false });
  assert.equal(await away.run(), false);
  assert.deepEqual(away.calls, []);
  // The person switched to another application while the window was asked.
  const switched = setup({ blurWhileAsking: true });
  assert.equal(await switched.run(), false);
  assert.deepEqual(switched.calls, ["ask"]);
});

test("a page without the keyboard, or a control other than the message box, gets nothing", async () => {
  const unfocused = setup({ page: false });
  assert.equal(await unfocused.run(), false);
  assert.deepEqual(unfocused.calls, []);
  const control = setup({ composer: false });
  assert.equal(await control.run(), false);
  assert.deepEqual(control.calls, ["ask"]);
});

test("a page the person loads takes the keyboard once it is shown, while the window is the one in use", () => {
  const focused: string[] = [];
  const win = (active: boolean) => ({ isDestroyed: () => false, isFocused: () => active, webContents: {} });
  const page = (closed = false) => ({ isDestroyed: () => closed, focus: () => void focused.push("page") });
  assert.equal(giveKeyboard(win(true) as never, page() as never, true), true);
  assert.deepEqual(focused, ["page"]);
  // Not drawn yet, another application in front, or the page gone: nothing moves.
  assert.equal(giveKeyboard(win(true) as never, page() as never, false), false);
  assert.equal(giveKeyboard(win(false) as never, page() as never, true), false);
  assert.equal(giveKeyboard(win(true) as never, page(true) as never, true), false);
  assert.deepEqual(focused, ["page"]);
});

test("a load the person made gives its page the keyboard when that tab is drawn, once, and gives up in time", () => {
  let time = 0;
  let active = true;
  const focused: string[] = [];
  const win = { isDestroyed: () => false, isFocused: () => active, webContents: {} };
  const page = (name: string) => ({ isDestroyed: () => false, focus: () => void focused.push(name) });
  const handoff = createKeyboardHandoff(win as never, 3000, () => time);
  // Nothing asked: drawing a tab moves nothing (the agent's loads never ask).
  assert.equal(handoff.offer("a", page("a") as never), false);
  // A new tab is drawn a moment after its load starts; another tab drawn meanwhile gets nothing.
  handoff.want("b");
  assert.equal(handoff.offer("a", page("a") as never), false);
  time = 500;
  assert.equal(handoff.offer("b", page("b") as never), true);
  assert.equal(handoff.offer("b", page("b") as never), false);
  assert.deepEqual(focused, ["b"]);
  // Drawn too late, the page leaves the keyboard where the person has it by then.
  handoff.want("c");
  time = 4000;
  assert.equal(handoff.offer("c", page("c") as never), false);
  // Another application in front: the load gives up, and coming back later does not revive it.
  handoff.want("d");
  active = false;
  assert.equal(handoff.offer("d", page("d") as never), false);
  active = true;
  assert.equal(handoff.offer("d", page("d") as never), false);
  assert.deepEqual(focused, ["b"]);
});
