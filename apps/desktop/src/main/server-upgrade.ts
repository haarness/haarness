import { ApiError, type Haarness } from "@haarness/sdk";

/**
 * Moving a project off a server of an earlier Haarness build.
 *
 * An updated Desktop (by itself, or by install.sh) finds the servers its
 * earlier copy started still running that copy's build: they work on their
 * own and outlive the app. Opening a project replaces such a server at once
 * when it is idle, but one with work under way (a turn, an open terminal, an
 * approval or question waiting, a running workflow) is used as it is, since
 * replacing it would end that work. This keeps an eye on those servers and
 * asks for the replacement as soon as the work is done, so a project is not
 * left on the earlier build until Desktop happens to restart while it idles.
 */

/** How often a server of an earlier build is asked whether it still works. */
export const UPGRADE_CHECK_MS = 15_000;

/** {@link UPGRADE_CHECK_MS}; end-to-end tests may shorten it. */
export function upgradeCheckMs(env: NodeJS.ProcessEnv = process.env) {
  if (env.HAARNESS_DESKTOP_TEST === "1") {
    const value = Number(env.HAARNESS_DESKTOP_UPGRADE_CHECK_MS);
    if (Number.isFinite(value) && value > 0) return value;
  }
  return UPGRADE_CHECK_MS;
}

/** What a server's own lists say about its work. */
export type ActivityClient = Pick<
  Haarness,
  "activeTurns" | "listPtys" | "listPermissions" | "listQuestions" | "listWorkflowRuns"
>;

/**
 * Whether a server has nothing under way that replacing it would end, read
 * as the CLI reads it before replacing one: a list too new for the server
 * (it answers 404 or 400) has nothing in it; any other failure is an unknown
 * state and counts as work.
 */
export async function serverIdle(client: ActivityClient) {
  const absent = <T>(read: Promise<T[]>) =>
    read.catch((error: unknown) => {
      if (error instanceof ApiError && (error.status === 404 || error.status === 400)) return [] as T[];
      throw error;
    });
  try {
    const [turns, terminals, approvals, questions, workflows] = await Promise.all([
      client.activeTurns({ timeoutMs: 5_000 }),
      absent(client.listPtys({ timeoutMs: 5_000 })),
      absent(client.listPermissions({ timeoutMs: 5_000 })),
      absent(client.listQuestions({ timeoutMs: 5_000 })),
      absent(client.listWorkflowRuns({ limit: 500 }, { timeoutMs: 5_000 })),
    ]);
    return (
      turns.length === 0 &&
      !terminals.some((terminal) => terminal.status === "running") &&
      approvals.length === 0 &&
      questions.length === 0 &&
      !workflows.some((run) => run.status === "running" || run.status === "paused")
    );
  } catch {
    return false;
  }
}

/**
 * Watches the connections whose server is of an earlier build. `pending`
 * names the client of a connection that still waits for its upgrade (none
 * once it is closed, remote or upgraded); when that server is idle, `replace`
 * opens the project again, which replaces it. A replacement that finds new
 * work, or fails, is tried again at the next check.
 */
export function upgradeWatcher<Client>(options: {
  pending: (id: string) => Client | undefined;
  idle: (client: Client) => Promise<boolean>;
  replace: (id: string) => Promise<void>;
  every: number;
  log?: (message: string) => void;
  schedule?: (run: () => void, ms: number) => ReturnType<typeof setTimeout>;
  cancel?: (timer: ReturnType<typeof setTimeout>) => void;
}) {
  const schedule = options.schedule ?? setTimeout;
  const cancel = options.cancel ?? clearTimeout;
  const timers = new Map<string, ReturnType<typeof setTimeout>>();
  function later(id: string) {
    const timer = schedule(() => {
      timers.delete(id);
      void check(id);
    }, options.every);
    (timer as { unref?: () => void }).unref?.();
    timers.set(id, timer);
  }
  async function check(id: string) {
    const client = options.pending(id);
    if (client === undefined) return;
    if (await options.idle(client).catch(() => false)) {
      try {
        await options.replace(id);
      } catch (error) {
        options.log?.(`Haarness: moving ${id} to this build failed: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
    if (options.pending(id) !== undefined && !timers.has(id)) later(id);
  }
  return {
    /** Starts watching a connection; one already watched is left as it is. */
    watch(id: string) {
      if (!timers.has(id)) later(id);
    },
    stop(id: string) {
      const timer = timers.get(id);
      if (timer !== undefined) cancel(timer);
      timers.delete(id);
    },
    get watching() {
      return [...timers.keys()];
    },
  };
}
