// Includes MIT-licensed code; see THIRD-PARTY-NOTICES.md.
import { Browser } from "./rpc";
import type { Protocol } from "devtools-protocol";
import { Encoding, Result, Schema } from "effect";
import { abortError, waitFor } from "./cdp";
import { createDiagnostics } from "./diagnostics";
import { createAgentCursor, cursorLabel, type CursorAction } from "./agent-cursor";
import { BrowserCodeError } from "./errors";
import { destinationOrigin, navigationResources, normalizeURL } from "./policy";
import type {
  CdpTransport,
  Page,
  PageOptions,
  PageResult,
  PageSurface,
} from "./types";

/** `name` is the accessible name from the snapshot, for the agent cursor's label. */
type Element = { backendID: number; frameID: string; sessionID?: string; name?: string };
/** A frame as `browser.frames` lists it, plus the security origin permission targets may use. */
type Frame = { id: string; parentID?: string; url: string; name: string; origin: string };
// One counter for every page in the process, so a ref never names an element of another tab.
let nextRef = 0;
/** The last element ref number minted in this process; a host that restarts passes it back as `refSeed`. */
export function lastRef() {
  return nextRef;
}
// Captures and downloads belong to the tab, not whichever document it now shows; navigate replaces it anyway.
const retainedOperations = new Set<Browser.Method>([
  "navigate",
  "files.list",
  "files.get",
  "trace.stop",
  "trace.analyze",
  "cpu.stop",
  "cpu.analyze",
  "heap.summary",
  "heap.query",
  "heap.object",
  "heap.compare",
]);

/**
 * Actions that act on the page: its scripts then run with a user's
 * activation, so the page could open a file chooser over the person's screen
 * while an agent works. For these the chooser is held back.
 */
const actingOperations = new Set<Browser.Method>([
  "navigate",
  "back",
  "forward",
  "reload",
  "evaluate",
  "click",
  "hover",
  "drag",
  "fill",
  "fill_form",
  "select",
  "check",
  "press",
  "scroll",
]);
/** How long choosers stay held after an action, for a page that opens one a moment later. */
const CHOOSER_GRACE_MS = 500;
const FILE_CHOOSER =
  "The page opened a file chooser, which Haarness kept from appearing: the person's screen stays theirs while an agent works. To choose a file, take a fresh browser.snapshot and call browser.files.upload with the file input's ref. Do not click the control again to open the chooser.";

/**
 * A document's permission resource when targets use origins: its HTTP(S)
 * security origin, else its URL's origin (a sandboxed frame's opaque origin
 * must not escape the gate of the site it loads), else the URL itself.
 */
function originResource(url: string, securityOrigin?: string) {
  if (securityOrigin && destinationOrigin(securityOrigin) === securityOrigin)
    return securityOrigin;
  return destinationOrigin(url) ?? url;
}

export function createPage<F extends Browser.HostFile = Browser.HostFile>(
  surface: PageSurface,
  cdp: CdpTransport,
  options: PageOptions<F>,
): Page<F> {
  if (options.refSeed !== undefined && options.refSeed > nextRef)
    nextRef = Math.floor(options.refSeed);
  // The page's listeners, so dispose can drop them from a transport that outlives it.
  const listeners = new Set<() => void>();
  const events: CdpTransport = {
    send: (method, params, sessionID) => cdp.send(method, params, sessionID),
    on(event, callback) {
      const off = cdp.on(event, callback);
      listeners.add(off);
      return () => {
        listeners.delete(off);
        off();
      };
    },
    dispose() {},
  };
  const files = options.files;
  const documents = new Map<string, { url: string; origin?: string }>();
  let mainFrameID: string | undefined;
  const sourceURLs = () =>
    [
      ...new Set([
        surface.url(),
        ...Array.from(documents.values(), (document) => document.url),
      ]),
    ].sort();
  const resource = (url: string, securityOrigin?: string) =>
    surface.resourceOrigins === "url"
      ? url
      : originResource(url, securityOrigin);
  const topResource = () => {
    const top = mainFrameID ? documents.get(mainFrameID) : undefined;
    return top
      ? originResource(top.url, top.origin)
      : originResource(surface.url());
  };
  // Resources a capture's bytes come from: every document since the last
  // navigation, or only the top one.
  const sources = (): string[] => {
    if (surface.resourceOrigins === "url")
      return surface.gateTopDocumentOnly ? [surface.url()] : sourceURLs();
    if (surface.gateTopDocumentOnly) return [topResource()];
    return [
      ...new Set([
        topResource(),
        ...Array.from(documents.values(), (document) =>
          originResource(document.url, document.origin),
        ),
      ]),
    ].sort();
  };
  const diagnostics = createDiagnostics(events, options.diagnostics);
  const cursor = createAgentCursor(surface.cursor);
  const tr = (key: string) => options.translate?.(key) ?? key;
  const label = (action: CursorAction, element: Element) => cursorLabel(action, element.name, tr);
  const refs = new Map<string, Element>();
  const sessions = new Map<string, string>();
  const parents = new Map<string, string>();
  const contexts = new Map<string, { id: number; sessionID?: string }>();
  const dialogs = new Set<() => void>();
  let dialog: { type: string; message: string; defaultValue: string } | null =
    null;
  let dialogURL = "";
  let dialogRevision = 0;
  let generation = options.generation ?? 0;
  let revision = 0;
  events.on("Page.frameNavigated", ({ frame }, sessionID) => {
    documents.set(frame.id, { url: frame.url, origin: frame.securityOrigin });
    if (!frame.parentId && !sessionID) mainFrameID = frame.id;
    revision++;
  });
  events.on("Page.frameDetached", ({ frameId }) => {
    documents.delete(frameId);
    revision++;
  });
  let closed = false;
  const state = (): Browser.Tab => ({
    id: options.id,
    url: surface.url().slice(0, 16_384),
    title: surface.title().slice(0, 2_048),
    loading: surface.loading(),
    canGoBack: surface.canGoBack(),
    canGoForward: surface.canGoForward(),
    generation,
  });
  const publish = () => {
    if (!closed) options.publish();
  };
  events.on("Runtime.executionContextCreated", ({ context }, sessionID) => {
    const aux = context.auxData as
      | { frameId?: string; isDefault?: boolean }
      | undefined;
    if (aux?.frameId && aux.isDefault)
      contexts.set(aux.frameId, { id: context.id, sessionID });
  });
  events.on(
    "Runtime.executionContextDestroyed",
    ({ executionContextId }, sessionID) => {
      contexts.forEach((context, key) => {
        if (
          context.id === executionContextId &&
          context.sessionID === sessionID
        )
          contexts.delete(key);
      });
    },
  );
  events.on(
    "Target.attachedToTarget",
    ({ sessionId, targetInfo }, parentSessionID) => {
      if (targetInfo.type !== "iframe") return;
      const parentID =
        targetInfo.parentFrameId ??
        Array.from(sessions).find(([, id]) => id === parentSessionID)?.[0];
      if (parentID) parents.set(targetInfo.targetId, parentID);
      sessions.set(targetInfo.targetId, sessionId);
      void Promise.all([
        diagnostics.enable(sessionId),
        cdp.send("Page.enable", {}, sessionId),
        cdp.send(
          "Target.setAutoAttach",
          {
            autoAttach: true,
            waitForDebuggerOnStart: false,
            flatten: true,
            filter: [{ type: "iframe", exclude: false }, { exclude: true }],
          },
          sessionId,
        ),
      ]).catch(() => undefined);
    },
  );
  events.on("Target.detachedFromTarget", ({ sessionId }) => {
    sessions.forEach((id, frameID) => {
      if (id === sessionId) {
        sessions.delete(frameID);
        parents.delete(frameID);
      }
    });
  });
  events.on("Page.javascriptDialogOpening", (event) => {
    dialogURL = event.url;
    dialogRevision++;
    dialog = {
      type: event.type,
      message: event.message.slice(0, Browser.MAX_TEXT),
      defaultValue: event.defaultPrompt ?? "",
    };
    dialogs.forEach((reject) => reject());
    publish();
  });
  events.on("Page.javascriptDialogClosed", () => {
    dialogRevision++;
    dialog = null;
    publish();
  });
  // While an agent's action runs, and for a moment after, a file chooser the
  // page opens is not shown (and is cancelled for the page); the action's
  // result says so. Between actions the person's own choosers open as usual.
  let chooser: { opened: boolean } | undefined;
  let chooserRelease: ReturnType<typeof setTimeout> | undefined;
  events.on("Page.fileChooserOpened", () => {
    if (chooser) chooser.opened = true;
  });
  const holdChoosers = (enabled: boolean) =>
    Promise.all(
      [undefined, ...sessions.values()].map((sessionID) =>
        cdp
          .send(
            "Page.setInterceptFileChooserDialog",
            enabled ? { enabled, cancel: true } : { enabled },
            sessionID,
          )
          .catch(() => undefined),
      ),
    );
  const releaseChoosers = (watch: { opened: boolean }) => {
    clearTimeout(chooserRelease);
    chooserRelease = setTimeout(() => {
      if (chooser !== watch || closed) return;
      chooser = undefined;
      void holdChoosers(false);
    }, CHOOSER_GRACE_MS);
  };
  // The page exists before its first load starts: a browser can report that
  // navigation's start synchronously, and the host's handler needs the page.
  let ready = Promise.resolve();
  const page: Page<F> = {
    state,
    get ready() {
      return ready;
    },
    sources,
    inspect,
    navigationStarted() {
      generation++;
      documents.clear();
      refs.clear();
      diagnostics.clear();
      options.onReset?.();
      publish();
    },
    async execute(command, signal) {
      if (!surface.supports(command.action.type))
        throw new BrowserCodeError(
          "unsupported",
          "This browser tool is not available in this browser.",
        );
      await ready;
      abortError(signal);
      if (closed)
        throw new Error(
          "Browser tab was closed. Call browser.tabs.list({}) and choose an existing tabID; do not reuse the closed tab's refs.",
        );
      if (dialog && command.action.type !== "dialog")
        throw new Error(
          'A JavaScript dialog is open. Inspect it with browser.dialog({tabID,action:"get"}), then explicitly accept or dismiss it before continuing.',
        );
      if (command.inspect)
        return { value: await inspect(command.action), files: [] };
      if (
        command.target &&
        JSON.stringify(await inspect(command.action)) !==
          JSON.stringify(command.target)
      )
        throw new Error(
          "Browser target changed while permission was pending. Take a fresh snapshot or listing and request the action again; it was not executed.",
        );
      if (
        command.generation !== undefined &&
        command.generation !== generation &&
        !retainedOperations.has(command.action.type)
      )
        throw new Error(
          "The document changed before this operation ran. Call browser.tabs.list({}) to check its current URL, then browser.snapshot({tabID}) for fresh refs. Reconsider the action before retrying on the new page.",
        );
      const modal = Promise.withResolvers<PageResult<F>>();
      const cancelled = Promise.withResolvers<never>();
      // The action underneath the race keeps running after a dialog wins it; it checks this
      // signal before each further step, so a validation alert on one field stops the rest.
      // A navigation is left alone: its beforeunload dialog is answered through browser.dialog
      // and the pending load then proceeds or not.
      const run = new AbortController();
      const cancel = () => {
        run.abort();
        cancelled.reject(
          new Error(
            "Browser operation was cancelled. Inspect the tab before deciding to repeat an action; cancellation does not undo changes already made.",
          ),
        );
      };
      signal.addEventListener("abort", cancel, { once: true });
      const reject = () => {
        if (command.action.type !== "navigate") run.abort();
        if (command.action.type === "click") {
          modal.resolve({
            value: {
              ...state(),
              dispatched: true,
              dialog,
              observation: {
                text: "",
                truncated: false,
                unavailable:
                  "A JavaScript dialog is open. Handle it with browser_dialog; do not repeat the click.",
              },
            },
            files: [],
          });
          return;
        }
        modal.reject(
          new Error(
            'A JavaScript dialog opened while the action was running. Inspect it with browser.dialog({tabID,action:"get"}) and accept or dismiss it. Do not repeat the original action just to close the dialog.',
          ),
        );
      };
      if (command.action.type !== "dialog") dialogs.add(reject);
      const watch = actingOperations.has(command.action.type)
        ? { opened: false }
        : undefined;
      const held = !!chooser;
      if (watch) {
        clearTimeout(chooserRelease);
        chooser = watch;
      }
      try {
        if (watch && !held) await holdChoosers(true);
        const outcome = await Promise.race([
          execute(command.action, command.files, run.signal, command.target),
          modal.promise,
          cancelled.promise,
        ]);
        const value = outcome.value;
        return watch?.opened && value && typeof value === "object" && !Array.isArray(value)
          ? { ...outcome, value: { ...value, fileChooser: FILE_CHOOSER } }
          : outcome;
      } finally {
        signal.removeEventListener("abort", cancel);
        dialogs.delete(reject);
        if (watch) releaseChoosers(watch);
      }
    },
    dispose() {
      if (closed) return;
      closed = true;
      clearTimeout(chooserRelease);
      listeners.forEach((off) => off());
      listeners.clear();
      refs.clear();
    },
  };
  ready = Promise.all([
    files.ready,
    ...(options.start ? [options.start(page)] : []),
    diagnostics.enable(),
    cdp.send("Page.enable"),
    cdp.send("DOM.enable"),
    cdp.send("Target.setAutoAttach", {
      autoAttach: true,
      waitForDebuggerOnStart: false,
      flatten: true,
      filter: [{ type: "iframe", exclude: false }, { exclude: true }],
    }),
  ]).then(() => undefined);
  return page;

  async function execute(
    action: Browser.Action,
    transfers: readonly Browser.HostFile[],
    signal: AbortSignal,
    approved?: Browser.Target,
  ): Promise<PageResult<F>> {
    const captureSources = sources();
    const transfer = (id: Browser.FileID) =>
      files.transfer(id, approved?.resources);
    const result = (
      value: unknown,
      attached: readonly F[] = [],
    ): PageResult<F> => {
      const json = Schema.decodeUnknownSync(Schema.Json)(value);
      if (JSON.stringify(json).length > 512_000)
        throw new Error(
          "Browser result exceeds 512000 JSON characters. Request fewer entries, reduce snapshot depth, or return only selected fields from the evaluation script. Repeating the same request will not reduce its output.",
        );
      return { value: json, files: attached };
    };
    if (options.extension?.methods.has(action.type))
      return options.extension.execute(action, {
        signal,
        state,
        sources: captureSources,
        transfer,
        result,
      });
    switch (action.type) {
      case "navigate": {
        await surface.navigate(normalizeURL(action.url), signal);
        abortError(signal);
        return result(state());
      }
      case "back":
      case "forward":
      case "reload":
      case "stop": {
        let started: void | Promise<void> = undefined;
        if (action.type === "back" && surface.canGoBack())
          started = surface.back();
        if (action.type === "forward" && surface.canGoForward())
          started = surface.forward();
        if (action.type === "reload") started = surface.reload();
        if (action.type === "stop") started = surface.stop();
        if (started) await started;
        if (action.type !== "stop")
          await waitFor(() => !surface.loading(), signal, 30_000);
        return result(state());
      }
      case "frames":
        return result({
          tab: state(),
          frames: (await frames()).map(({ origin: _origin, ...frame }) => frame),
        });
      case "snapshot":
      case "find":
        return result({ tab: state(), ...(await snapshot(action)) });
      case "evaluate": {
        const context = action.frameID
          ? contexts.get(action.frameID)
          : undefined;
        if (action.frameID && !context)
          throw new Error(
            "Frame context is unavailable. Call browser.frames({tabID}) and use a current frameID from this tab, or omit frameID to target the main frame.",
          );
        const value = await cdp.send(
          "Runtime.evaluate",
          {
            expression: action.script,
            contextId: context?.id,
            awaitPromise: true,
            returnByValue: true,
            userGesture: true,
          },
          context?.sessionID,
        );
        if (value.exceptionDetails)
          throw new Error(
            `Page JavaScript threw an exception. Check the script and frameID; inspect the page before repeating code with side effects. Details: ${(value.exceptionDetails.exception?.description ?? value.exceptionDetails.text).slice(0, 800)}`,
          );
        abortError(signal);
        return result({ tab: state(), value: value.result.value ?? null });
      }
      case "click": {
        const element = target(action.ref);
        const position = await point(element);
        await cursor.move(position, label("click", element));
        cursor.click(position);
        await click(
          element,
          action.button ?? "left",
          action.count ?? 1,
          action.modifiers,
        );
        // A modal may have already returned this action's result. Do not evaluate
        // page JavaScript while that dialog is waiting for an explicit response.
        abortError(signal);
        // A click can submit without navigating. Return visible evidence so an unchanged
        // URL is not mistaken for a failed action and retried as a second submission.
        return result({
          ...state(),
          dispatched: true,
          observation: await observe(element.frameID),
        });
      }
      case "hover": {
        const element = target(action.ref);
        const position = await point(element);
        await cursor.move(position, label("point", element));
        await cdp.send("Input.dispatchMouseEvent", {
          type: "mouseMoved",
          ...position,
        });
        break;
      }
      case "drag": {
        const source = target(action.from);
        const destination = target(action.to);
        await point(destination);
        const from = await point(source);
        const box = await rect(destination);
        const to = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
        await cursor.move(from, label("drag", source));
        cursor.click(from);
        const html5 = await call(
          source,
          "function() { return this.draggable; }",
        );
        let data: Protocol.Input.DragData | undefined;
        const off = events.on("Input.dragIntercepted", (event) => {
          data = event.data;
        });
        await cdp.send("Input.setInterceptDrags", { enabled: true });
        await cdp.send("Input.dispatchMouseEvent", {
          type: "mouseMoved",
          ...from,
        });
        await cdp.send("Input.dispatchMouseEvent", {
          type: "mousePressed",
          ...from,
          button: "left",
          buttons: 1,
          clickCount: 1,
        });
        try {
          await cursor.move(to, label("drop", destination));
          for (let i = 1; i <= 10; i++) {
            abortError(signal);
            await cdp.send("Input.dispatchMouseEvent", {
              type: "mouseMoved",
              x: from.x + ((to.x - from.x) * i) / 10,
              y: from.y + ((to.y - from.y) * i) / 10,
              button: "left",
              buttons: 1,
            });
          }
          if (html5) await waitFor(() => data !== undefined, signal, 2_000);
          if (data) {
            for (const type of ["dragEnter", "dragOver", "drop"])
              await cdp.send("Input.dispatchDragEvent", { type, ...to, data });
          }
        } finally {
          off();
          await cdp.send("Input.setInterceptDrags", { enabled: false });
          await cdp.send("Input.dispatchMouseEvent", {
            type: "mouseReleased",
            ...to,
            button: "left",
            clickCount: 1,
          });
        }
        break;
      }
      case "fill": {
        const element = target(action.ref);
        await cursor.move(await point(element), label("type", element), true);
        await fill(element, action.text, signal);
        break;
      }
      case "fill_form":
        for (const field of action.fields) {
          abortError(signal);
          const element = target(field.ref);
          const position = await point(element);
          if (field.type === "text") {
            await cursor.move(position, label("type", element), true);
            await fill(element, field.value, signal);
          }
          if (field.type === "select") {
            await cursor.move(position, label("choose", element));
            cursor.click(position);
            await select(element, field.values);
          }
          if (field.type === "check") {
            await cursor.move(position, label(field.checked ? "check" : "uncheck", element));
            cursor.click(position);
            await check(element, field.checked);
          }
        }
        break;
      case "select": {
        const element = target(action.ref);
        const position = await point(element);
        await cursor.move(position, label("choose", element));
        cursor.click(position);
        await select(element, action.values);
        break;
      }
      case "check": {
        const element = target(action.ref);
        const position = await point(element);
        await cursor.move(position, label(action.checked ? "check" : "uncheck", element));
        cursor.click(position);
        await check(element, action.checked);
        break;
      }
      case "press":
        cursor.note(tr("Pressing {key}").replace("{key}", action.key));
        await key(action.key);
        break;
      case "scroll": {
        const bounds = await surface.viewport();
        await cursor.move(
          { x: bounds.width / 2, y: bounds.height / 2 },
          tr(action.deltaY < 0 ? "Scrolling up" : action.deltaY > 0 ? "Scrolling down" : "Scrolling"),
        );
        await cdp.send("Input.dispatchMouseEvent", {
          type: "mouseWheel",
          x: bounds.width / 2,
          y: bounds.height / 2,
          deltaX: action.deltaX ?? 0,
          deltaY: action.deltaY,
        });
        break;
      }
      case "wait": {
        if (action.condition !== "load" && !action.text)
          throw new Error(
            'browser.wait requires non-empty text for condition "text" or "textGone". Use condition "load" without text to wait for loading.',
          );
        await waitFor(
          async () => {
            if (action.condition === "load") return !surface.loading();
            const context = action.frameID
              ? contexts.get(action.frameID)
              : undefined;
            if (action.frameID && !context)
              throw new Error(
                "Frame context is unavailable. Call browser.frames({tabID}) and use a frameID from this tab.",
              );
            const value = await cdp.send(
              "Runtime.evaluate",
              {
                expression: `document.body?.innerText.includes(${JSON.stringify(action.text)}) ?? false`,
                returnByValue: true,
                contextId: context?.id,
              },
              context?.sessionID,
            );
            return (
              Boolean(value.result.value) === (action.condition === "text")
            );
          },
          signal,
          action.timeoutMs,
        ).catch((error) => {
          if (signal.aborted) throw error;
          throw new Error(
            `browser.wait failed for condition ${JSON.stringify(action.condition)} (timeoutMs: ${action.timeoutMs ?? 10_000}). Inspect browser.snapshot({tabID}) and check text/frameID before retrying; timeoutMs can be increased up to 30000 for a genuinely slow page. Details: ${error instanceof Error ? error.message : String(error)}`,
          );
        });
        break;
      }
      case "screenshot": {
        if (action.ref && action.fullPage)
          throw new Error(
            "Choose either ref for an element screenshot or fullPage:true for the whole page. Remove the other argument before retrying.",
          );
        await surface.readyForCapture(signal);
        const element = action.ref
          ? await rect(target(action.ref), true)
          : undefined;
        const metrics = await cdp.send("Page.getLayoutMetrics");
        const bounds = element
          ? {
              ...element,
              x: element.x + metrics.cssVisualViewport.pageX,
              y: element.y + metrics.cssVisualViewport.pageY,
            }
          : action.fullPage
            ? metrics.cssContentSize
            : {
                x: metrics.cssVisualViewport.pageX,
                y: metrics.cssVisualViewport.pageY,
                width: metrics.cssVisualViewport.clientWidth,
                height: metrics.cssVisualViewport.clientHeight,
              };
        const pixelRatio = await surface.pixelRatio();
        const scale = Math.min(
          1,
          (action.maxWidth ?? 2000) / (bounds.width * pixelRatio),
        );
        if (bounds.width <= 0 || bounds.height <= 0)
          throw new Error(
            "Element or page has no visible screenshot area. Take a fresh snapshot and choose a visible element, or omit ref to capture the viewport.",
          );
        if (
          bounds.width * bounds.height * (scale * pixelRatio) ** 2 >
          16_000_000
        )
          throw new Error(
            "Screenshot exceeds 16 megapixels; capture an element or use a smaller maxWidth.",
          );
        const format = action.format ?? "png";
        const capture = await cursor.hidden(() =>
          cdp.send("Page.captureScreenshot", {
            format,
            quality: format === "png" ? undefined : (action.quality ?? 80),
            captureBeyondViewport: true,
            clip: { ...bounds, scale },
          }),
        );
        const id = await files.save(
          `screenshot.${format}`,
          `image/${format}`,
          Result.getOrThrow(Encoding.decodeBase64(capture.data)),
          [...captureSources, ...sources()],
        );
        return result({ tab: state() }, [await transfer(id)]);
      }
      case "dialog": {
        if (action.action !== "get") {
          if (!dialog)
            throw new Error(
              'This tab has no JavaScript dialog to handle. browser.dialog({tabID,action:"get"}) returns null when none is open; continue without accepting or dismissing one.',
            );
          await cdp.send("Page.handleJavaScriptDialog", {
            accept: action.action === "accept",
            promptText: action.promptText,
          });
          dialog = null;
        }
        return result({ tab: state(), dialog });
      }
      case "files.upload":
      case "files.drop": {
        if (!transfers.length)
          throw new Error(
            "Upload command has no file bytes. Supply server-local paths to browser.files.upload/drop; do not call the desktop RPC directly with desktop paths. If paths were supplied, report a client/server transfer mismatch.",
          );
        const local = await options.input.stage(transfers, sources());
        const element = target(action.ref);
        if (action.type === "files.upload")
          await cdp.send(
            "DOM.setFileInputFiles",
            { files: local, backendNodeId: element.backendID },
            element.sessionID,
          );
        if (action.type === "files.drop") {
          const position = await point(element);
          for (const type of ["dragEnter", "dragOver", "drop"])
            await cdp.send("Input.dispatchDragEvent", {
              type,
              ...position,
              data: { items: [], files: local, dragOperationsMask: 1 },
            });
        }
        break;
      }
      case "files.list":
        return result({ tab: state(), files: files.list() });
      case "files.get":
        return result({ tab: state() }, [await transfer(action.fileID)]);
      case "console":
        return result({ tab: state(), ...diagnostics.console(action) });
      case "network.list":
        return result({ tab: state(), ...diagnostics.list(action) });
      case "network.get":
        return result({ tab: state(), ...(await diagnostics.get(action)) });
      default:
        throw new Error(
          "This operation was routed to a page instead of the tab manager. Report a desktop/plugin routing mismatch; changing tab IDs or repeating the operation will not fix it.",
        );
    }
    abortError(signal);
    return result(state());
  }

  async function inspect(action: Browser.Action): Promise<Browser.Target> {
    // Most CDP queries cannot run while a JavaScript dialog blocks the renderer.
    if (action.type === "dialog")
      return {
        resources: [resource(dialog ? dialogURL : surface.url())],
        key: `${generation}:${dialogRevision}:${Boolean(dialog)}`,
      };
    const fileIDs =
      action.type === "heap.compare"
        ? [action.before, action.after]
        : "fileID" in action
          ? [action.fileID]
          : [];
    if (fileIDs.length)
      return {
        resources: [
          ...new Set(fileIDs.flatMap((id) => files.get(id).resources)),
        ].sort(),
        key: JSON.stringify(fileIDs),
      };
    if (action.type === "network.get")
      return {
        resources: [resource(diagnostics.info(action.id).url)],
        key: action.id,
      };
    const custom = options.extension?.methods.has(action.type)
      ? options.extension.inspect?.(action)
      : undefined;
    if (custom) return custom;
    if (action.type === "back" || action.type === "forward") {
      const entry = surface.historyEntry(action.type === "back" ? -1 : 1);
      const destination = entry.url ?? surface.url();
      return {
        resources: [resource(destination)],
        key: JSON.stringify([generation, revision, entry.index, destination]),
      };
    }
    const tree = await frames();
    tree.forEach((frame) =>
      documents.set(frame.id, { url: frame.url, origin: frame.origin }),
    );
    if (tree[0]) mainFrameID = tree[0].id;
    const refs =
      action.type === "drag"
        ? [action.from, action.to]
        : action.type === "fill_form"
          ? action.fields.map((field) => field.ref)
          : "ref" in action && action.ref
            ? [action.ref]
            : [];
    const selected = refs.map(target);
    const frameIDs = selected.length
      ? selected.map((element) => element.frameID)
      : "frameID" in action && action.frameID
        ? [action.frameID]
        : [];
    const urls = frameIDs.map((id) => {
      const frame = tree.find((frame) => frame.id === id);
      if (!frame)
        throw new Error(
          "Frame is unavailable. Call browser.frames({tabID}) and use a current frameID.",
        );
      return resource(frame.url, frame.origin);
    });
    const capture = [
      "screenshot",
      "lighthouse",
      "trace.start",
      "cpu.start",
      "heap.snapshot",
    ].includes(action.type);
    return {
      resources: [
        ...new Set(
          action.type === "navigate"
            ? navigationResources(action.url, {
                origins: surface.resourceOrigins === "securityOrigin",
                twin: surface.wwwTwin,
              })
            : capture
              ? sources()
              : urls.length
                ? urls
                : [
                    surface.resourceOrigins === "url" || !tree[0]
                      ? resource(surface.url())
                      : resource(tree[0].url, tree[0].origin),
                  ],
        ),
      ].sort(),
      key: JSON.stringify([generation, revision, selected]),
    };
  }

  function target(ref: Browser.Ref): Element {
    const value = refs.get(ref.replace(/^@/, ""));
    if (!value)
      throw new Error(
        "Element ref is stale or belongs to another tab. Call browser.snapshot({tabID}) and use a ref from that tab's newest snapshot. Do not reuse refs after navigation or a newer snapshot.",
      );
    return value;
  }

  async function frames() {
    const root = await cdp.send("Page.getFrameTree");
    const result: Frame[] = [];
    const walk = (tree: Protocol.Page.FrameTree, parentID?: string) => {
      if (!result.some((frame) => frame.id === tree.frame.id))
        result.push({
          id: tree.frame.id,
          ...(tree.frame.parentId || parentID
            ? { parentID: tree.frame.parentId ?? parentID }
            : {}),
          url: tree.frame.url,
          name: tree.frame.name ?? "",
          origin: tree.frame.securityOrigin,
        });
      tree.childFrames?.forEach((child) => walk(child, tree.frame.id));
    };
    walk(root.frameTree);
    const children = await Promise.all(
      Array.from(sessions, async ([id, sessionID]) => ({
        id,
        tree: await cdp
          .send("Page.getFrameTree", {}, sessionID)
          .catch(() => undefined),
      })),
    );
    children.forEach(({ id, tree }) => {
      if (tree)
        walk(tree.frameTree, parents.get(id) ?? root.frameTree.frame.id);
    });
    return result;
  }

  async function call(
    element: Element,
    functionDeclaration: string,
    args: unknown[] = [],
  ) {
    const object = await cdp.send(
      "DOM.resolveNode",
      { backendNodeId: element.backendID },
      element.sessionID,
    );
    const objectId = object.object.objectId;
    if (!objectId)
      throw new Error(
        "Element is no longer available. Call browser.snapshot({tabID}) and use a fresh ref; the page may have replaced the element.",
      );
    try {
      const result = await cdp.send(
        "Runtime.callFunctionOn",
        {
          objectId,
          functionDeclaration,
          arguments: args.map((value) => ({ value })),
          returnByValue: true,
          awaitPromise: true,
          userGesture: true,
        },
        element.sessionID,
      );
      if (result.exceptionDetails)
        throw new Error(
          result.exceptionDetails.exception?.description ??
            result.exceptionDetails.text,
        );
      return result.result.value as unknown;
    } finally {
      await cdp
        .send("Runtime.releaseObject", { objectId }, element.sessionID)
        .catch(() => undefined);
    }
  }

  async function rect(element: Element, scroll = false) {
    if (scroll) {
      await cdp.send(
        "DOM.scrollIntoViewIfNeeded",
        { backendNodeId: element.backendID },
        element.sessionID,
      );
      // Let the host force a compositor update after scrolling (PageSurface.repaint).
      await surface.repaint?.();
    }
    const shape = Schema.Struct({
      x: Schema.Finite,
      y: Schema.Finite,
      width: Schema.Finite,
      height: Schema.Finite,
    });
    const value = {
      ...Schema.decodeUnknownSync(shape)(
        await call(
          element,
          "function() { const r = this.getBoundingClientRect(); return {x:r.x,y:r.y,width:r.width,height:r.height}; }",
        ),
      ),
    };
    const tree = await frames();
    let frame = tree.find((frame) => frame.id === element.frameID);
    while (frame?.parentID) {
      const parent = {
        frameID: frame.parentID,
        sessionID: sessionFor(frame.parentID, tree),
      };
      const owner = await cdp.send(
        "DOM.getFrameOwner",
        { frameId: frame.id },
        parent.sessionID,
      );
      // A CSS transform on the iframe scales its content box; the child's own coordinates are unscaled.
      const box = Schema.decodeUnknownSync(
        Schema.Struct({
          ...shape.fields,
          scaleX: Schema.Finite,
          scaleY: Schema.Finite,
        }),
      )(
        await call(
          { backendID: owner.backendNodeId, ...parent },
          "function() { const r = this.getBoundingClientRect(); const sx = this.offsetWidth ? r.width / this.offsetWidth : 1; const sy = this.offsetHeight ? r.height / this.offsetHeight : 1; return {x:r.x+this.clientLeft*sx,y:r.y+this.clientTop*sy,width:r.width,height:r.height,scaleX:sx,scaleY:sy}; }",
        ),
      );
      value.x = box.x + value.x * box.scaleX;
      value.y = box.y + value.y * box.scaleY;
      value.width *= box.scaleX;
      value.height *= box.scaleY;
      frame = tree.find((item) => item.id === frame?.parentID);
    }
    return value;
  }

  // Same-process child frames have no CDP target of their own; the nearest ancestor with one owns them.
  function sessionFor(
    frameID: string,
    tree: { id: string; parentID?: string }[],
  ) {
    let id: string | undefined = frameID;
    while (id && !sessions.has(id))
      id = tree.find((frame) => frame.id === id)?.parentID;
    return id ? sessions.get(id) : undefined;
  }

  async function point(element: Element) {
    const box = await rect(element, true);
    return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  }

  async function click(
    element: Element,
    button = "left",
    count = 1,
    modifiers: readonly string[] = [],
  ) {
    const position = await point(element);
    const flags = modifiers.reduce(
      (mask, key) =>
        mask | ({ Alt: 1, Control: 2, Meta: 4, Shift: 8 }[key] ?? 0),
      0,
    );
    await cdp.send("Input.dispatchMouseEvent", {
      type: "mouseMoved",
      ...position,
      modifiers: flags,
    });
    for (let clickCount = 1; clickCount <= count; clickCount++) {
      await cdp.send("Input.dispatchMouseEvent", {
        type: "mousePressed",
        ...position,
        button,
        clickCount,
        modifiers: flags,
      });
      await cdp.send("Input.dispatchMouseEvent", {
        type: "mouseReleased",
        ...position,
        button,
        clickCount,
        modifiers: flags,
      });
    }
  }

  async function fill(element: Element, value: string, signal: AbortSignal) {
    const kind = await call(
      element,
      "function() { if (this.disabled || this.readOnly) return; if (this instanceof HTMLTextAreaElement || this.isContentEditable) return 'text'; if (!(this instanceof HTMLInputElement)) return; if (['date','time','datetime-local','month','week'].includes(this.type)) return 'structured'; if (!['file','checkbox','radio','button','submit','reset','image','hidden','range','color'].includes(this.type)) return 'text'; }",
    );
    if (!kind)
      throw new Error(
        "Target is not an enabled editable text field. Take a fresh snapshot and choose a textbox; use browser.select for dropdowns, browser.check for checkboxes/radios, or browser.files.upload for file inputs.",
      );
    // Keyboard input cannot compose a date or time control's value; Chromium clears a malformed one.
    if (kind === "structured") {
      await call(
        element,
        "function(value) { const previous = this.value; this.focus(); this.value = value; if (this.value !== value) { this.value = previous; throw new Error('The ' + this.type + ' input rejected this value and keeps its previous one. Use its required format, for example 2026-09-07 for date, 14:45 for time, 2026-09-07T14:45 for datetime-local, 2026-09 for month, or 2026-W37 for week.'); } this.dispatchEvent(new Event('input',{bubbles:true})); this.dispatchEvent(new Event('change',{bubbles:true})); }",
        [value],
      );
      return;
    }
    await cdp.send(
      "DOM.focus",
      { backendNodeId: element.backendID },
      element.sessionID,
    );
    // Focusing this field blurs the previous one; a validation dialog from that blur must stop here.
    abortError(signal);
    await selectAll();
    await key("Backspace");
    abortError(signal);
    await cdp.send("Input.insertText", { text: value });
  }

  async function selectAll() {
    const mode = surface.selectAll;
    if ("keys" in mode) return key(mode.keys);
    // An editing command selects the focused field's text whatever the
    // platform's shortcut; Chrome on macOS ignores a synthetic ⌘A.
    const params = { key: "a", code: "KeyA", windowsVirtualKeyCode: 65 };
    await cdp.send("Input.dispatchKeyEvent", {
      type: "rawKeyDown",
      ...params,
      commands: [...mode.commands],
    });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", ...params });
  }

  async function select(element: Element, values: readonly string[]) {
    await call(
      element,
      `function(values) { if (!(this instanceof HTMLSelectElement) || this.disabled) throw new Error('Target is not an enabled HTML select. Take a fresh snapshot and choose an enabled dropdown ref.'); if (!this.multiple && values.length !== 1) throw new Error('This dropdown accepts exactly one value; pass a one-item values array.'); for (const value of values) if (!Array.from(this.options).some(option => option.value === value && !option.disabled)) throw new Error('Option value was not found or is disabled. Inspect option values with browser.evaluate before retrying browser.select; values are not visible labels.'); for (const option of this.options) option.selected = values.includes(option.value); this.dispatchEvent(new Event('input',{bubbles:true})); this.dispatchEvent(new Event('change',{bubbles:true})); }`,
      [values],
    );
  }

  async function check(element: Element, checked: boolean) {
    const current = await call(
      element,
      "function(checked) { if (!(this instanceof HTMLInputElement) || !['checkbox','radio'].includes(this.type) || this.disabled) throw new Error('Target is not an enabled checkbox or radio. Take a fresh snapshot and choose the correct ref.'); if (this.type === 'radio' && this.checked && !checked) throw new Error('A selected radio cannot be cleared by clicking it. Select a different radio in its group instead.'); return this.checked; }",
      [checked],
    );
    if (current !== checked) await click(element);
    if (
      (await call(element, "function() { return this.checked; }")) !== checked
    )
      throw new Error(
        "The page did not keep the requested checked state. Inspect the current snapshot and page validation before retrying; do not blindly toggle the control again.",
      );
  }

  async function key(chord: string) {
    if (!chord)
      throw new Error(
        "A key is required. Use a named key such as Enter or ArrowDown, a single character, or a chord such as Control+A.",
      );
    const parts = (chord.endsWith("+") ? chord.slice(0, -1) : chord).split("+");
    const key = parts.pop() || "+";
    const modifiers = parts.reduce((mask, key) => {
      const bit = { Alt: 1, Control: 2, Meta: 4, Shift: 8 }[key];
      if (!bit)
        throw new Error(
          `Unknown key modifier ${JSON.stringify(key)}. Supported modifiers are Alt, Control, Meta, and Shift; for example Control+A. Use Meta for macOS command shortcuts.`,
        );
      return mask | bit;
    }, 0);
    const codes: Record<string, number> = {
      Enter: 13,
      Tab: 9,
      Escape: 27,
      Backspace: 8,
      Delete: 46,
      ArrowUp: 38,
      ArrowDown: 40,
      ArrowLeft: 37,
      ArrowRight: 39,
      PageUp: 33,
      PageDown: 34,
      Home: 36,
      End: 35,
      Space: 32,
    };
    const code =
      codes[key] ??
      (key.length === 1
        ? key.toUpperCase().charCodeAt(0)
        : /^F([1-9]|1[0-2])$/.test(key)
          ? 111 + Number(key.slice(1))
          : undefined);
    if (code === undefined)
      throw new Error(
        `Unknown key ${JSON.stringify(key)}. Use Enter, Tab, Escape, Backspace, Delete, ArrowUp/Down/Left/Right, PageUp/Down, Home, End, Space, F1–F12, or one character. Use browser.fill for text.`,
      );
    // Named keys need their character data too: Enter submits forms and inserts newlines only with "\r".
    const text =
      key === "Enter"
        ? "\r"
        : key === "Space"
          ? " "
          : key.length === 1
            ? key
            : undefined;
    const params = {
      key: key === "Space" ? " " : key,
      windowsVirtualKeyCode: code,
      modifiers,
      ...(text !== undefined && !(modifiers & 6) ? { text } : {}),
    };
    await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", ...params });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", ...params });
  }

  async function observe(frameID: string) {
    const context = contexts.get(frameID);
    if (!context)
      return {
        text: "",
        truncated: false,
        unavailable:
          "Document changed; take a fresh snapshot before another action.",
      };
    try {
      const output = await cdp.send(
        "Runtime.evaluate",
        {
          expression:
            "(()=>{const text=document.body?.innerText??'';return {text:text.slice(0,12000),truncated:text.length>12000}})()",
          contextId: context.id,
          returnByValue: true,
          timeout: 750,
        },
        context.sessionID,
      );
      if (output.exceptionDetails || !output.result.value)
        throw new Error("Observation unavailable");
      return output.result.value as { text: string; truncated: boolean };
    } catch {
      return {
        text: "",
        truncated: false,
        unavailable:
          "The click was dispatched. Inspect or wait for the page; do not repeat it merely to check success.",
      };
    }
  }

  async function snapshot(
    action: Extract<Browser.Action, { type: "snapshot" | "find" }>,
  ) {
    const tree = await frames();
    const selected =
      action.type === "snapshot" && action.ref ? target(action.ref) : undefined;
    const frameID = selected?.frameID ?? action.frameID ?? tree[0]?.id;
    if (!frameID || !tree.some((frame) => frame.id === frameID))
      throw new Error(
        "Frame is unavailable. Call browser.frames({tabID}) and use a current frameID from this tab; omit frameID for the main frame.",
      );
    const sessionID = sessionFor(frameID, tree);
    const depth = action.type === "snapshot" ? (action.depth ?? 8) : 8;
    const ax = await cdp.send(
      "Accessibility.getFullAXTree",
      // CDP depth counts ignored layout nodes. Apply the requested depth to the
      // visible outline below, so a shallow snapshot still describes form controls.
      { frameId: frameID },
      sessionID,
    );
    const nodes = new Map(ax.nodes.map((node) => [node.nodeId, node]));
    const root = selected
      ? ax.nodes.find((node) => node.backendDOMNodeId === selected.backendID)
      : ax.nodes[0];
    if (!root)
      throw new Error(
        "Element is absent from this frame's accessibility snapshot. Retry browser.snapshot with the same tabID and no ref to refresh the frame, then choose a returned ref.",
      );
    refs.clear();
    const lines: string[] = [];
    let truncated = false;
    let visited = 0;
    const walk = async (
      node: Protocol.Accessibility.AXNode,
      level: number,
    ): Promise<void> => {
      if (level > depth || lines.length >= 500 || ++visited > 10_000) {
        truncated = true;
        return;
      }
      const role = String(node.role?.value ?? "node")
        .replace(/[^a-zA-Z0-9_-]/g, "")
        .slice(0, 40);
      const properties = new Map(
        node.properties?.map((property) => [
          property.name,
          property.value.value,
        ]) ?? [],
      );
      const visible =
        !node.ignored &&
        !(
          ["none", "generic", "LabelText", "form"].includes(role) &&
          !node.name?.value
        );
      let structuredInput = false;
      if (visible) {
        // Drop zones, images and headings can be interaction targets without being
        // keyboard-focusable. Text-only AX nodes do not represent targetable elements.
        const actionable = ![
          "RootWebArea",
          "StaticText",
          "InlineTextBox",
          "LineBreak",
        ].includes(role);
        const ref = actionable && node.backendDOMNodeId ? `e${++nextRef}` : "";
        const element = node.backendDOMNodeId
          ? { backendID: node.backendDOMNodeId, frameID, sessionID, name: node.name?.value as string | undefined }
          : undefined;
        if (ref && element) refs.set(ref, element);
        const flags = (
          ["checked", "disabled", "expanded", "selected"] as const
        ).flatMap((name) =>
          properties.has(name) ? [`${name}=${properties.get(name)}`] : [],
        );
        let fieldValue = node.value?.value;
        if (element && ["Date", "DateTime", "InputTime"].includes(role)) {
          const input = await call(
            element,
            "function() { return this instanceof HTMLInputElement ? { type: this.type, value: this.value } : null; }",
          ).catch(() => null);
          const format =
            input &&
            typeof input === "object" &&
            "type" in input &&
            typeof input.type === "string" &&
            "value" in input &&
            typeof input.value === "string" &&
            (
              {
                date: "YYYY-MM-DD",
                time: "HH:mm",
                "datetime-local": "YYYY-MM-DDTHH:mm",
                month: "YYYY-MM",
                week: "YYYY-Www",
              } as Record<string, string>
            )[input.type];
          if (format) {
            structuredInput = true;
            fieldValue = (input as { value: string }).value;
            flags.push("format=" + JSON.stringify(format));
          }
        }
        if (
          fieldValue !== undefined &&
          (fieldValue !== "" || structuredInput)
        ) {
          let value = String(fieldValue).slice(0, 1000);
          if (["textbox", "searchbox"].includes(role) && element) {
            // Input values help verify a fill, but password values must stay redacted.
            const attributes = await cdp
              .send(
                "DOM.describeNode",
                { backendNodeId: element.backendID },
                sessionID,
              )
              .then((result) => result.node.attributes)
              .catch(() => undefined);
            if (
              !attributes ||
              attributes.some(
                (key, index) =>
                  index % 2 === 0 &&
                  key.toLowerCase() === "type" &&
                  attributes[index + 1]?.toLowerCase() === "password",
              )
            )
              value = "[redacted]";
          }
          flags.push("value=" + JSON.stringify(value));
        }
        const box =
          action.type === "snapshot" && action.boxes && ref && element
            ? await rect(element).catch(() => undefined)
            : undefined;
        lines.push(
          `${"  ".repeat(level)}${ref ? `@${ref} ` : ""}[${role}] ${JSON.stringify(
            String(node.name?.value ?? "")
              .replace(/\s+/g, " ")
              .slice(0, 300),
          )} ${flags.join(" ")}${box ? ` box=${JSON.stringify(box)}` : ""}`,
        );
      }
      // Native date/time subfields are shadow DOM internals. The fill tool takes
      // the complete formatted value on the input, not one of those subfields.
      if (
        structuredInput ||
        ["textbox", "searchbox"].includes(role) ||
        properties.get("editable")
      )
        return;
      for (const childID of node.childIds ?? []) {
        const child = nodes.get(childID);
        if (child) await walk(child, level + (visible ? 1 : 0));
        else truncated = true;
      }
    };
    await walk(root, 0);
    const content = (
      action.type === "find"
        ? lines.filter((line) =>
            line.toLowerCase().includes(action.text.toLowerCase()),
          )
        : lines
    ).join("\n");
    return {
      content: content.slice(0, Browser.MAX_TEXT),
      truncated: truncated || content.length > Browser.MAX_TEXT,
    };
  }
}
