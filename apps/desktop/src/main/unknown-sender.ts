import { ApiError } from "@haarness/sdk";

/** Runs `call`, and once more when the server refused it because it could not
 * tell which program on this computer sent it: that answer closed the
 * connection, so the second try comes on a new one. A second refusal stands,
 * in the server's own words. */
export async function onceMoreForUnknownSender<T>(call: () => Promise<T>): Promise<T> {
  try {
    return await call();
  } catch (error) {
    if (error instanceof ApiError && error.code === "unknown_sender") return await call();
    throw error;
  }
}
