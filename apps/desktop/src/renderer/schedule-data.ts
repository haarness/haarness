import type { Thread } from "@haarness/sdk";

export type Trigger =
  | { type: "interval"; seconds: number }
  | { type: "cron"; expression: string }
  | { type: "once"; at: string };
export interface Schedule {
  id: string;
  name: string;
  prompt: string;
  enabled: boolean;
  workspace: string;
  backend: string;
  trigger: Trigger;
  model?: string;
  agent?: string;
  variant?: string;
  timezone?: string;
}
export const weekdays = [
  "Sunday",
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
];
export const units = [
  { value: 60, label: "Minutes" },
  { value: 3600, label: "Hours" },
  { value: 86400, label: "Days" },
  { value: 1, label: "Seconds" },
];
export type Frequency =
  | "interval"
  | "daily"
  | "weekdays"
  | "weekly"
  | "once"
  | "cron";
export function localDate(value: string | Date) {
  const date = new Date(value);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000)
    .toISOString()
    .slice(0, 16);
}
export function triggerForm(trigger?: Trigger) {
  const form = {
    frequency: "daily" as Frequency,
    amount: 1,
    unit: 3600,
    time: "09:00",
    day: 1,
    at: localDate(new Date(Date.now() + 3600000)),
    cron: "0 9 * * *",
  };
  if (!trigger) return form;
  if (trigger.type === "interval") {
    const unit = [86400, 3600, 60, 1].find(
      (value) => trigger.seconds % value === 0,
    )!;
    return {
      ...form,
      frequency: "interval" as const,
      amount: trigger.seconds / unit,
      unit,
    };
  }
  if (trigger.type === "once")
    return { ...form, frequency: "once" as const, at: localDate(trigger.at) };
  const match = trigger.expression
    .trim()
    .match(/^(\d{1,2}) (\d{1,2}) \* \* (\*|1-5|[0-7])$/);
  if (!match || +match[1] > 59 || +match[2] > 23)
    return { ...form, frequency: "cron" as const, cron: trigger.expression };
  return {
    ...form,
    frequency: (match[3] === "*"
      ? "daily"
      : match[3] === "1-5"
        ? "weekdays"
        : "weekly") as Frequency,
    time: `${match[2].padStart(2, "0")}:${match[1].padStart(2, "0")}`,
    day: match[3] === "*" || match[3] === "1-5" ? 1 : Number(match[3]) % 7,
    cron: trigger.expression,
  };
}
export function triggerArgs(
  form: ReturnType<typeof triggerForm>,
): [string, string] {
  if (form.frequency === "interval") {
    const seconds = form.amount * form.unit;
    if (!Number.isSafeInteger(seconds) || seconds < 60)
      throw new Error("Choose an interval of at least one minute.");
    return ["--every", seconds + "s"];
  }
  if (form.frequency === "once") {
    const date = new Date(form.at);
    if (!Number.isFinite(date.getTime()) || date.getTime() < Date.now() + 60000)
      throw new Error("Choose a date and time at least one minute from now.");
    return ["--at", date.toISOString()];
  }
  if (form.frequency === "cron") {
    if (!form.cron.trim()) throw new Error("Enter a cron expression.");
    return ["--cron", form.cron.trim()];
  }
  if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(form.time))
    throw new Error("Choose a valid time.");
  if (
    form.frequency === "weekly" &&
    (!Number.isInteger(form.day) || form.day < 0 || form.day > 6)
  )
    throw new Error("Choose a day of the week.");
  const [hour, minute] = form.time.split(":").map(Number);
  return [
    "--cron",
    `${minute} ${hour} * * ${form.frequency === "daily" ? "*" : form.frequency === "weekdays" ? "1-5" : form.day}`,
  ];
}
export function describeSchedule(trigger: Trigger, t: (key: string) => string = (key) => key, locale = "en") {
  const form = triggerForm(trigger);
  if (locale !== "en") {
    if (form.frequency === "interval")
      return `${t("At an interval")} · ${form.amount} ${t(units.find((unit) => unit.value === form.unit)!.label)}`;
    if (form.frequency === "once")
      return `${t("Once")} · ${new Date((trigger as { at: string }).at).toLocaleString(locale, { dateStyle: "medium", timeStyle: "short" })}`;
    if (form.frequency === "cron") return `${t("Custom cron")} · ${form.cron}`;
    return `${form.frequency === "daily" ? t("Every day") : form.frequency === "weekdays" ? t("Weekdays") : t("Weekly") + " · " + t(weekdays[form.day])} · ${form.time}`;
  }
  if (form.frequency === "interval")
    return `Every ${form.amount} ${units
      .find((unit) => unit.value === form.unit)!
      .label.toLowerCase()
      .replace(/s$/, form.amount === 1 ? "" : "s")}`;
  if (form.frequency === "once")
    return `Once · ${new Date((trigger as { at: string }).at).toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}`;
  if (form.frequency === "cron") return `Custom · ${form.cron}`;
  return `${form.frequency === "daily" ? "Every day" : form.frequency === "weekdays" ? "Weekdays" : "Every " + weekdays[form.day]} at ${form.time}`;
}

/** Strict legacy recognition, retained for runs created before schedule metadata. */
export function scheduled(thread: Thread) {
  if (thread.schedule) return thread.schedule;
  if (thread.parentId) return;
  const match = thread.title.match(
    /^Schedule: (.+) @ (\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:\d\d))$/,
  );
  if (match && Number.isFinite(Date.parse(match[2])))
    return { id: "legacy:" + match[1], name: match[1] };
}
export function splitSchedules<
  T extends { key: string; location: { id: string }; thread: Thread },
>(rows: T[]) {
  const chats: T[] = [];
  const groups = new Map<string, { id: string; name: string; runs: T[] }>();
  const known = new Map<string, Set<string>>();
  for (const row of rows) {
    if (!row.thread.schedule) continue;
    const key = JSON.stringify([row.location.id, row.thread.schedule.name]);
    const ids = known.get(key) ?? new Set<string>();
    ids.add(row.thread.schedule.id);
    known.set(key, ids);
  }
  for (const row of rows) {
    const origin = scheduled(row.thread);
    if (!origin) {
      chats.push(row);
      continue;
    }
    // Two checkouts can contain imported runs with the same schedule ID.
    const matches = known.get(JSON.stringify([row.location.id, origin.name]));
    const identity =
      !row.thread.schedule && matches?.size === 1 ? [...matches][0] : origin.id;
    const id = JSON.stringify([row.location.id, identity]);
    const group = groups.get(id) ?? { id, name: origin.name, runs: [] as T[] };
    group.runs.push(row);
    groups.set(id, group);
  }
  for (const group of groups.values())
    group.runs.sort(
      (a, b) =>
        b.thread.createdAt.localeCompare(a.thread.createdAt) ||
        b.key.localeCompare(a.key),
    );
  return {
    chats,
    groups: [...groups.values()].sort(
      (a, b) =>
        b.runs[0].thread.createdAt.localeCompare(a.runs[0].thread.createdAt) ||
        a.id.localeCompare(b.id),
    ),
  };
}
