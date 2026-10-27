import { createEffect, createSignal, onCleanup, on } from "solid-js";
import type { VoiceEvent } from "../shared";
import { errorLine } from "./i18n";
import { cancelled } from "./error-text";

export function insertDictation(
  base: string,
  start: number,
  end: number,
  text: string,
) {
  const spoken = text.trim();
  if (!spoken) return { text: base, caret: start };
  const before = base.slice(0, start),
    after = base.slice(end);
  const prefix = before && !/\s$/.test(before) ? " " : "";
  const suffix = after && !/^[\s.,!?;:)]/.test(after) ? " " : "";
  return {
    text: before + prefix + spoken + suffix + after,
    caret: before.length + prefix.length + spoken.length,
  };
}

export function createDictation(options: {
  owner: () => string;
  visible: () => boolean;
  supported: () => boolean;
  textarea: () => HTMLTextAreaElement;
  read: () => string;
  write: (text: string) => void;
}) {
  const [status, setStatus] = createSignal<
    "idle" | "starting" | "listening" | "finishing"
  >("idle");
  const [error, setError] = createSignal("");
  const [preparation, setPreparation] = createSignal("Preparing dictation…");
  let run:
    | { id: string; owner: string; base: string; start: number; end: number }
    | undefined;
  const active = () => status() !== "idle";
  function cancel() {
    const previous = run;
    run = undefined;
    setStatus("idle");
    if (previous)
      void window.haarness.voice("cancel", previous.id).catch(() => {});
  }
  function receive(id: string | undefined, event: VoiceEvent) {
    const current = run;
    if (!current || current.id !== id || current.owner !== options.owner())
      return;
    if (event.type === "preparing") {
      if (status() === "starting") setPreparation(event.message);
    } else if (event.type === "started") {
      if (status() === "starting") setStatus("listening");
    } else if (event.type === "transcript") {
      const value = insertDictation(
        current.base,
        current.start,
        current.end,
        event.text,
      );
      options.write(value.text);
      options.textarea()?.setSelectionRange(value.caret, value.caret);
    } else if (event.type === "error") {
      setError(event.message);
      cancel();
    } else if (event.type === "ended") {
      run = undefined;
      setStatus("idle");
    }
  }
  const unlisten = window.haarness.onEvent((event) => {
    if (event.kind === "voice") receive(event.id, event.data as VoiceEvent);
  });
  async function toggle() {
    if (active()) {
      if (!run) return;
      setStatus("finishing");
      await window.haarness.voice("stop", run.id).catch((error) => {
        if (!cancelled(error)) setError(errorLine(error));
        cancel();
      });
      return;
    }
    if (!options.supported() || !options.visible()) return;
    const element = options.textarea();
    const current = {
      id: crypto.randomUUID(),
      owner: options.owner(),
      base: options.read(),
      start: element.selectionStart,
      end: element.selectionEnd,
    };
    run = current;
    setError("");
    setPreparation("Preparing dictation…");
    setStatus("starting");
    element.focus();
    try {
      await window.haarness.voice("start", current.id, navigator.language);
    } catch (error) {
      if (run !== current) return;
      if (!cancelled(error)) setError(errorLine(error));
      cancel();
    }
  }
  createEffect(
    on(
      () => options.owner() + ":" + options.visible(),
      () => {
        cancel();
        setError("");
      },
    ),
  );
  const shortcut = (event: KeyboardEvent) => {
    if (
      options.visible() &&
      options.supported() &&
      (event.metaKey || event.ctrlKey) &&
      event.shiftKey &&
      event.code === "KeyM"
    ) {
      event.preventDefault();
      void toggle();
    }
  };
  document.addEventListener("keydown", shortcut);
  onCleanup(() => {
    cancel();
    unlisten();
    document.removeEventListener("keydown", shortcut);
  });
  return {
    status,
    preparation,
    active,
    error,
    clearError: () => setError(""),
    cancel,
    toggle,
  };
}
