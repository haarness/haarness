import { test } from "node:test";
import assert from "node:assert/strict";
import { returnKeyboard } from "../src/main/browser-keyboard";

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
