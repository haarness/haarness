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
export function reasoningLabel(value: string) {
  return value
    ? value
        .replace(/[_-]/g, " ")
        .replace(/^./, (letter) => letter.toUpperCase())
    : "Model default";
}
