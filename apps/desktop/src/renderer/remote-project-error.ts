/** Keep server diagnostics out of the form, but preserve the useful next step. */
export function remoteProjectError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  if (/401|unauthori[sz]ed|authentication required/i.test(message))
    return "Couldn’t sign in. Check this server’s username and password in Settings.";
  if (
    /active turn|still starting|different authentication|busy on a different version|busy with a different project configuration|whether it still works/i.test(
      message,
    )
  )
    return "This project’s server is busy. Wait a moment and try again.";
  if (
    /incompatible|needs an upgrade|does not support remote project/i.test(
      message,
    )
  )
    return "Update Haarness on the server, then try opening this project again.";
  if (
    /timeout|timed out|did not become ready|aborted due to timeout/i.test(
      message,
    )
  )
    return "The server took too long to open this project. Try again in a moment.";
  if (
    /permission denied|cannot be read|access is denied|403|forbidden/i.test(
      message,
    )
  )
    return "Haarness doesn’t have access to this folder. Check its permissions on the server.";
  if (
    /not a directory|no such file|directory is missing|folder is unavailable|directory does not exist|path does not exist/i.test(
      message,
    )
  )
    return "This folder is no longer available. Choose another folder or check the path.";
  if (/cancelled/i.test(message))
    return "Opening was cancelled. Try again when you’re ready.";
  return "Couldn’t open this project. Make sure the server is running, then try again.";
}
