export interface ReviewDraft {
  id: string;
  text: string;
  target?: { connection: string; thread: string };
}
export type ReviewRequest =
  | { action: "load"; scope: string }
  | { action: "save" | "pin"; scope: string; anchor: string; draft: ReviewDraft }
  | { action: "remove"; scope: string; anchor: string; id: string }
  | { action: "deliver"; scope: string; anchor: string; draft: ReviewDraft; note: string; current: string };
type Receipt = { destination: string; before: string; after: string; committed: boolean };
export type ReviewResponse = { drafts: Record<string, ReviewDraft>; text?: string };

/** One persistence transaction contains both the composer text and its receipt.
 * A failed write may be retried without appending a comment twice. */
export function createReviewStorage(values: Record<string, unknown>, persist: () => Promise<void>) {
  let writes = Promise.resolve();
  return (request: ReviewRequest): Promise<ReviewResponse> => {
    const result = writes.catch(() => {}).then(async () => {
      const key = "review-drafts:v1:" + request.scope;
      const drafts = { ...(values[key] as Record<string, ReviewDraft> | undefined) };
      let text: string | undefined;
      let delivery: Receipt | undefined;
      if (request.action === "save" || (request.action === "pin" && drafts[request.anchor]?.id === request.draft.id)) drafts[request.anchor] = request.draft;
      if (request.action === "remove" && drafts[request.anchor]?.id === request.id)
        delete drafts[request.anchor];
      if (request.action === "deliver") {
        const target = request.draft.target;
        if (!target) throw new Error("Review delivery has no conversation");
        const receipt = "review-delivery:v1:" + request.draft.id;
        const destination = "draft:" + target.connection + ":" + target.thread;
        const previous = values[receipt] as Receipt | undefined;
        if (previous && previous.destination !== destination) throw new Error("This comment belongs to a different conversation");
        if (!previous || !previous.committed) {
          text = previous && (request.current === previous.before || request.current === previous.after)
            ? previous.after : [request.current, request.note].filter(Boolean).join("\n\n");
          values[destination] = text;
          delivery = { destination, before: request.current, after: text, committed: true };
          values[receipt] = delivery;
        } else text = typeof values[destination] === "string" ? values[destination] as string : "";
        // Only the delivered revision may be removed. New edits survive.
        if (drafts[request.anchor]?.id === request.draft.id) delete drafts[request.anchor];
      }
      if (request.action !== "load") {
        const previous = values[key];
        values[key] = drafts;
        try { await persist(); }
        catch (error) {
          // Keep the source recoverable after a failed delivery write. The
          // in-memory receipt still fences retries until persistence succeeds.
          if (request.action === "deliver") {
            values[key] = previous;
            if (delivery) delivery.committed = false;
          }
          throw error;
        }
      }
      return { drafts, text };
    });
    writes = result.then(() => {}, () => {});
    return result;
  };
}
