/** The raised log level, as `haarness logs level --json` reports it. */
export interface LogLevelStatus {
  /** The level every Haarness process here follows now; null when each keeps its own. */
  level: string | null;
  /** When it goes back by itself; null until it is stopped (or when none is raised). */
  until: string | null;
  untilStopped: boolean;
}
/** What `haarness logs export --json` wrote. */
export interface ExportedLogs {
  path: string;
  files: number;
  skipped: number;
}
/** How long Detailed logging lasts before it turns itself off. */
export const DETAILED_LOGGING = "30m";
/** The zip's suggested name: the day it was saved, as people file it. */
export function logsFileName(now: Date) {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `haarness-logs-${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}.zip`;
}
/** Whole minutes left before `until`, rounded up; 0 once it has passed. */
export function minutesLeft(until: string | null, now: number) {
  if (!until) return 0;
  const left = Date.parse(until) - now;
  return Number.isFinite(left) && left > 0 ? Math.ceil(left / 60_000) : 0;
}
