import type { FileDiff } from "@haarness/sdk";

/** Per-review-generation cache. Limits both patch retention and concurrent
 * reads; old generations never populate a newly selected server or source. */
export class ReviewCache {
  private entries = new Map<string, FileDiff>();
  private pending = new Map<string, Promise<FileDiff | undefined>>();
  private queue: (() => void)[] = [];
  private active = 0;
  private disposed = false;
  constructor(
    private read: (path: string) => Promise<FileDiff | undefined>,
    private bytes = 24 * 1024 * 1024,
    private limit = 24,
  ) {}

  load(path: string): Promise<FileDiff | undefined> {
    const hit = this.entries.get(path);
    if (hit) {
      this.entries.delete(path);
      this.entries.set(path, hit);
      return Promise.resolve(hit);
    }
    const pending = this.pending.get(path);
    if (pending) return pending;
    const promise = new Promise<FileDiff | undefined>((resolve, reject) => {
      this.queue.push(() => {
        if (this.disposed) {
          resolve(undefined);
          return;
        }
        this.active++;
        void this.read(path)
          .then((value) => {
            if (value && !this.disposed) {
              this.entries.set(path, value);
              let size = Array.from(this.entries.values()).reduce(
                (n, file) =>
                  n +
                  2 *
                    ((file.patch?.length ?? 0) +
                      (file.before?.length ?? 0) +
                      (file.after?.length ?? 0)),
                0,
              );
              while (this.entries.size > this.limit || size > this.bytes) {
                const [key, file] = this.entries.entries().next().value!;
                size -=
                  2 *
                  ((file.patch?.length ?? 0) +
                    (file.before?.length ?? 0) +
                    (file.after?.length ?? 0));
                this.entries.delete(key);
              }
            }
            resolve(this.disposed ? undefined : value);
          }, reject)
          .finally(() => {
            this.active--;
            this.pending.delete(path);
            this.drain();
          });
      });
    });
    this.pending.set(path, promise);
    this.drain();
    return promise;
  }
  private drain() {
    while (this.active < 3 && this.queue.length) this.queue.shift()!();
  }
  dispose() {
    this.disposed = true;
    this.entries.clear();
    this.drain();
  }
}
