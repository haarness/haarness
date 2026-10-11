/**
 * The native port to `haarness browser native-host`. The service worker is the
 * only place that opens it (a second port would start a second bridge that
 * replaces the first on every server). An open port keeps the worker alive;
 * when it closes the worker retries on a timer, and a one-minute alarm wakes a
 * worker Chrome stopped in the meantime.
 */
import { createReassembler, isChunk } from "./chunks";
import { NATIVE_HOST, PROTOCOL, parseFromBridge, type FromBridge, type ToBridge } from "./protocol";
import type { NativePort, Platform } from "./platform";
import { disconnectReason, reconnectDelay, type Connection } from "./status";

export interface BridgeHandlers {
  hello(): ToBridge;
  message(message: FromBridge): void;
  connection(connection: Connection, failures: number): void;
}

export const RECONNECT_ALARM = "reconnect";

export function createBridge(platform: Pick<Platform, "connectNative" | "alarm">, handlers: BridgeHandlers) {
  let port: NativePort | undefined;
  let generation = 0;
  let attempt = 0;
  let failures = 0;
  let watching = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let connection: Connection = "connecting";
  /** Set by the owner's first connect(), once `hello` has its state; nothing else opens the port before. */
  let enabled = false;
  const reassembler = createReassembler();

  const set = (next: Connection) => {
    connection = next;
    handlers.connection(connection, failures);
  };

  function connect() {
    enabled = true;
    clearTimeout(timer);
    timer = undefined;
    if (port) return;
    const current = ++generation;
    reassembler.reset();
    if (connection !== "not-set-up" && connection !== "bridge-exited") set("connecting");
    port = platform.connectNative(NATIVE_HOST, {
      message(raw) {
        if (current !== generation) return;
        let message: unknown = raw;
        if (isChunk(raw)) {
          try {
            message = reassembler.push(raw);
          } catch {
            return;
          }
          if (message === undefined) return;
        }
        const parsed = parseFromBridge(message);
        if (!parsed) return;
        if (parsed.type === "bridge") {
          attempt = 0;
          failures = 0;
          platform.alarm(RECONNECT_ALARM, null);
          set(parsed.protocol === PROTOCOL ? "connected" : "mismatch");
        }
        if (connection === "mismatch" && parsed.type !== "bridge") return;
        handlers.message(parsed);
      },
      disconnect(error) {
        if (current !== generation) return;
        port = undefined;
        failures++;
        set(disconnectReason(error));
        schedule();
      },
    });
    port.post(handlers.hello());
  }

  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(connect, reconnectDelay(watching > 0, attempt++));
    platform.alarm(RECONNECT_ALARM, 1);
  }

  return {
    connect,
    connection: () => connection,
    failures: () => failures,
    connected: () => !!port && connection === "connected",
    send(message: ToBridge) {
      if (!port || connection !== "connected") return false;
      port.post(message);
      return true;
    },
    /** A popup or welcome page opened (+1) or closed (−1); while one is open, retry every 2 s. */
    watch(delta: 1 | -1) {
      const before = watching;
      watching = Math.max(0, watching + delta);
      if (enabled && !port && before === 0 && watching > 0) {
        attempt = 0;
        connect();
      }
    },
    /** "Check again" or the reconnect alarm. */
    retry() {
      if (enabled && !port) connect();
    },
    /** Closes the port and stops retrying. */
    close() {
      clearTimeout(timer);
      generation++;
      port?.close();
      port = undefined;
    },
    /** Starts a fresh bridge so it reads a new `hello` (the profile label changed). */
    restart() {
      if (!enabled) return;
      if (!port) return connect();
      generation++;
      port.close();
      port = undefined;
      set("connecting");
      connect();
    },
  };
}

export type Bridge = ReturnType<typeof createBridge>;
