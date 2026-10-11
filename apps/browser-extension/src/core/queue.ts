/**
 * One chat's commands run one at a time, in arrival order, like Desktop's
 * per-conversation queue; different chats run side by side. Each command can
 * be cancelled by id whether it is running or still waiting, and Pause cancels
 * everything at once.
 */

interface Job {
  key: string;
  abort: AbortController;
}

export function createQueues() {
  const tails = new Map<string, Promise<unknown>>();
  const jobs = new Map<string, Job>();

  return {
    /**
     * Queues `task` behind the chat's earlier commands. The task always runs —
     * with an aborted signal if it was cancelled while waiting — so the caller
     * answers every command exactly once.
     */
    run<T>(key: string, id: string, task: (signal: AbortSignal) => Promise<T>): Promise<T> {
      const job: Job = { key, abort: new AbortController() };
      jobs.set(id, job);
      const previous = tails.get(key) ?? Promise.resolve();
      const result = previous
        .catch(() => {})
        .then(() => task(job.abort.signal))
        .finally(() => {
          if (jobs.get(id) === job) jobs.delete(id);
          if (tails.get(key) === settled) tails.delete(key);
        });
      const settled = result.catch(() => {});
      tails.set(key, settled);
      return result;
    },
    cancel(id: string) {
      const job = jobs.get(id);
      job?.abort.abort();
      return !!job;
    },
    /** Cancels every running and waiting command (Pause, Stop). */
    cancelAll() {
      for (const job of jobs.values()) job.abort.abort();
    },
    /** Commands not yet answered, for one chat or in total. */
    pending(key?: string) {
      let count = 0;
      for (const job of jobs.values()) if (key === undefined || job.key === key) count++;
      return count;
    },
  };
}

export type Queues = ReturnType<typeof createQueues>;
