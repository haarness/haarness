/** The provider catalog supplies the supported order; never synthesize levels
 * from a model name or assume that all reasoning-capable models share a scale.
 */
export function reasoningLevels(levels?: readonly string[] | null) {
  return [
    ...new Set(
      (levels ?? []).filter(
        (level) => typeof level === "string" && level.length > 0,
      ),
    ),
  ];
}
export function validReasoning(
  value: string,
  levels?: readonly string[] | null,
) {
  return reasoningLevels(levels).includes(value) ? value : "";
}
/** The effort a turn names to run at the model's own default over a configured
 * `defaults.reasoning_effort` (a server reports that one as
 * `defaultReasoningEffort`). Elsewhere naming no effort means the same, and an
 * older server refuses the name, so it is sent only where a level is configured. */
export const MODEL_DEFAULT_EFFORT = "default";
/** Every provider effort Haarness knows, lightest first (the server's scale). */
const scale = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
/** The level of `levels` a model runs a turn asking for `effort` at, as the
 * server fits it: the level itself, else the nearest on the scale, the stronger
 * one on a tie (levels off the scale rank strongest). "" when it lists none. */
export function nearestReasoning(
  effort: string,
  levels?: readonly string[] | null,
) {
  const known = reasoningLevels(levels);
  if (known.includes(effort)) return effort;
  const rank = (level: string) =>
    scale.includes(level) ? scale.indexOf(level) : scale.length;
  const wanted = rank(effort);
  let best = "";
  let nearest = Infinity;
  for (const level of known) {
    const distance = Math.abs(rank(level) - wanted);
    if (distance < nearest || (distance === nearest && rank(level) > rank(best))) {
      best = level;
      nearest = distance;
    }
  }
  return best;
}
/** What the reasoning control shows for a model with `levels` when `effort`
 * is chosen ("" for no choice) and the server configures `configured`: its
 * stops, Model default first, and the level a turn runs at ("" for the
 * model's own default), which with no choice is the configured one. */
export function reasoningChoice(
  effort: string,
  levels?: readonly string[] | null,
  configured?: string | null,
) {
  const known = reasoningLevels(levels);
  const fitted = configured ? nearestReasoning(configured, known) : "";
  // Over a configured level the model's default is a choice of its own;
  // without one, no choice means it.
  const modelDefault = configured ? MODEL_DEFAULT_EFFORT : "";
  const steps = [modelDefault, ...known];
  const current = effort ? validReasoning(effort, known) : fitted;
  const unavailable =
    !!effort &&
    effort !== MODEL_DEFAULT_EFFORT &&
    validReasoning(effort, known) !== effort;
  return {
    steps,
    modelDefault,
    current,
    index: Math.max(0, steps.indexOf(current || modelDefault)),
    unavailable,
    /** The level in use is the configured one. */
    configured: !!fitted && current === fitted && !unavailable,
  };
}
/** The choice that stands for a model with `levels`: a level it lists, and
 * the model's own default, which every model has, while a level is
 * `configured` for it to override; "" otherwise. */
export function carriedReasoning(
  effort: string,
  levels?: readonly string[] | null,
  configured?: string | null,
) {
  if (effort === MODEL_DEFAULT_EFFORT) return configured ? effort : "";
  return validReasoning(effort, levels);
}
/** The `reasoningEffort` a request names for the choice `effort`, if any: the
 * model's own default only to a server that configures a level. */
export function requestedReasoning(effort?: string, configured?: string | null) {
  if (!effort || (effort === MODEL_DEFAULT_EFFORT && !configured)) return undefined;
  return effort;
}
/** Names for the levels providers share; an English catalog key, translated where it renders. */
const levelNames: Record<string, string> = {
  none: "None",
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
};
export function reasoningLabel(value: string) {
  if (!value || value === MODEL_DEFAULT_EFFORT) return "Model default";
  return Object.hasOwn(levelNames, value)
    ? levelNames[value]
    : value.replace(/[_-]/g, " ").replace(/^./, (letter) => letter.toUpperCase());
}
