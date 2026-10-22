/**
 * A failed Review load that Review explains in its own words: the folder is
 * not a Git repository, or a partial clone has not downloaded files the
 * review needs (the server never downloads them while reviewing). Any other
 * failure shows the server's text.
 */
export function reviewProblem(text: string): "repository" | "download" | undefined {
  if (/not (?:inside a git worktree|a git repository)|workspace is not a directory/i.test(text))
    return "repository";
  if (/has not downloaded the files the review needs/i.test(text)) return "download";
  return undefined;
}
