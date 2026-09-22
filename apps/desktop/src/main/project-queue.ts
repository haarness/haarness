// A project's runtime and its saved options change as one transaction. Reading
// saved options inside the queue prevents background refreshes from restoring
// settings that a newer explicit request has already changed.
export function createProjectQueue() {
  const pending = new Map<string, Promise<unknown>>();
  return function run<T>(id: string, operation: () => Promise<T>): Promise<T> {
    const previous = pending.get(id) ?? Promise.resolve();
    const next = previous.catch(() => {}).then(operation);
    pending.set(id, next);
    void next
      .finally(() => {
        if (pending.get(id) === next) pending.delete(id);
      })
      .catch(() => {});
    return next;
  };
}
