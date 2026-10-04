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
  if (!value) return "Model default";
  return Object.hasOwn(levelNames, value)
    ? levelNames[value]
    : value.replace(/[_-]/g, " ").replace(/^./, (letter) => letter.toUpperCase());
}
