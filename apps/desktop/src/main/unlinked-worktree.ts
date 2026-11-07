/**
 * Whether a removal may go on although the worktree's own server could not
 * start: only when the server refused to start it because its link to its
 * repository broke (its `.git` names a Git folder that is gone). Such a
 * worktree runs nothing a removal could cut off, and the server only forgets
 * it, leaving its folder as it is. Every other failure (a timeout, a busy
 * older server, a trust question) is reported as it came, with its own
 * reason. The words are the server's own refusal.
 */
export function removalSurvivesFailedStart(method: string, error: unknown) {
  return (
    method === "removeWorkspace" &&
    String(error instanceof Error ? error.message : error).includes(
      "the worktree no longer links to its repository",
    )
  );
}
