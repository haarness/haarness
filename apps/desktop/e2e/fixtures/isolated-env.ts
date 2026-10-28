/** What every Desktop a spec starts gets, whatever the runner's environment
 * holds: no usage statistics, Haarness Cloud on a closed loopback port, and
 * schedules written into the test's own folders but never handed to launchd,
 * systemd or Task Scheduler (an isolated data folder does not move those). A
 * spec that talks to a Cloud stand-in sets its own HAARNESS_CLOUD_API_URL. */
export const ISOLATED = {
  HAARNESS_TELEMETRY: "0",
  HAARNESS_CLOUD_API_URL: "http://127.0.0.1:9",
  HAARNESS_SCHEDULE_SKIP_INSTALL: "1",
};

/**
 * The runner's environment without the person's own HAARNESS_* settings and
 * the variables named in `drop`, with `ISOLATED` put back: leaving out every
 * HAARNESS_* setting also left out what keeps a test away from Cloud.
 */
export function isolatedEnv(drop: string[] = []): Record<string, string> {
  const env = Object.fromEntries(
    Object.entries(process.env).filter(
      ([key, value]) => value !== undefined && !key.startsWith("HAARNESS_") && !drop.includes(key),
    ),
  ) as Record<string, string>;
  return Object.assign(env, ISOLATED);
}
