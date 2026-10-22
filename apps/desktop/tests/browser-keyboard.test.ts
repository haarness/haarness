import { test } from "node:test";
import assert from "node:assert/strict";
import { createKeyboardHandoff, returnKeyboard } from "../src/main/browser-keyboard";

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

/** A window whose own keyboard is in a field (`typing`) or not, and a clock the test moves. */
function handoff(state: { typing?: boolean } = {}) {
  let time = 0;
  let active = true;
  const focused: string[] = [];
  const win = {
    isDestroyed: () => false,
    isFocused: () => active,
    webContents: {
      focus: () => void focused.push("window"),
      executeJavaScript: async (code: string) => {
        assert.match(code, /data-browser-address/);
        return state.typing ?? false;
      },
    },
  };
  const page = (name: string) => ({ isDestroyed: () => false, focus: () => void focused.push(name) });
  return {
    focused,
    page,
    keyboard: createKeyboardHandoff(win as never, 3000, () => time),
    at: (ms: number) => void (time = ms),
    away: (value: boolean) => void (active = !value),
  };
}

test("a load the person made gives its page the keyboard when that tab is drawn, once", async () => {
  const { focused, page, keyboard, at } = handoff();
  // Nothing asked: drawing a tab moves nothing (the agent's loads never ask).
  assert.equal(await keyboard.offer("a", page("a") as never), false);
  // A new tab names itself once its command starts; another tab drawn meanwhile gets nothing.
  keyboard.want();
  assert.equal(await keyboard.offer("a", page("a") as never), false);
  keyboard.bind("b");
  at(500);
  assert.equal(await keyboard.offer("b", page("b") as never), true);
  assert.equal(await keyboard.offer("b", page("b") as never), false);
  assert.deepEqual(focused, ["b"]);
});

test("a load's time runs from the request, so one that waited behind the agent leaves the keyboard alone", async () => {
  const { focused, page, keyboard, at } = handoff();
  keyboard.want("c");
  // The command ran (and its page was drawn) only after the agent's slow one.
  at(4000);
  assert.equal(await keyboard.offer("c", page("c") as never), false);
  at(4100);
  assert.equal(await keyboard.offer("c", page("c") as never), false);
  assert.deepEqual(focused, []);
});

test("a press in the window after the request, or a field there with the keyboard, keeps the keyboard where the person is", async () => {
  const pressed = handoff();
  pressed.keyboard.want("d");
  pressed.keyboard.input();
  assert.equal(await pressed.keyboard.offer("d", pressed.page("d") as never), false);
  // The press came before a later request, which still counts.
  pressed.keyboard.want("d");
  assert.equal(await pressed.keyboard.offer("d", pressed.page("d") as never), true);
  // Typing in the message box, find or a terminal (not the address bar): the request is dropped.
  const typing = handoff({ typing: true });
  typing.keyboard.want("e");
  assert.equal(await typing.keyboard.offer("e", typing.page("e") as never), false);
  assert.equal(await typing.keyboard.offer("e", typing.page("e") as never), false);
  assert.deepEqual(typing.focused, []);
});

test("the keyboard never moves while another application is in front, and coming back does not revive the request", async () => {
  const { focused, page, keyboard, away } = handoff();
  keyboard.want("f");
  away(true);
  assert.equal(await keyboard.offer("f", page("f") as never), false);
  away(false);
  assert.equal(await keyboard.offer("f", page("f") as never), false);
  assert.deepEqual(focused, []);
});
