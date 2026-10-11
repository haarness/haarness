/** Small timing helpers shared by the Chrome platform. */
import { CANCELLED } from "../../core/errors";

/**
 * Settles with `promise`, or rejects when `signal` aborts, or — with a
 * timeout — rejects once that many milliseconds pass.
 */
export function abortable<T>(promise: Promise<T>, signal: AbortSignal, timeoutMs?: number): Promise<T> {
  if (signal.aborted) return Promise.reject(new Error(CANCELLED));
  return new Promise<T>((resolve, reject) => {
    const timer =
      timeoutMs === undefined ? undefined : setTimeout(() => finish(() => reject(new Error(`Timed out after ${timeoutMs} ms`))), timeoutMs);
    const abort = () => finish(() => reject(new Error(CANCELLED)));
    const finish = (settle: () => void) => {
      clearTimeout(timer);
      signal.removeEventListener("abort", abort);
      settle();
    };
    signal.addEventListener("abort", abort, { once: true });
    promise.then(
      (value) => finish(() => resolve(value)),
      (error) => finish(() => reject(error)),
    );
  });
}

/** Settles with `promise`'s value, or with undefined once `ms` pass or it rejects. */
export function within<T>(promise: Promise<T>, ms: number): Promise<T | undefined> {
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(undefined), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      () => {
        clearTimeout(timer);
        resolve(undefined);
      },
    );
  });
}
