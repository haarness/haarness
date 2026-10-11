/*
 * The Haarness mark (from `brand/`): two posts joined by a strap that
 * overhangs them, drawn in `currentColor` on the app-icon tile.
 */

/** The mark on its tile, as on the toolbar button. */
export function Tile(props: { size: number; class?: string }) {
  return (
    <svg
      class={"brand-tile" + (props.class ? " " + props.class : "")}
      width={props.size}
      height={props.size}
      viewBox="0 0 64 64"
      aria-hidden="true"
    >
      <rect width="64" height="64" rx="15" fill="var(--tile, #151413)" />
      <rect x="0.75" y="0.75" width="62.5" height="62.5" rx="14.25" fill="none" stroke="#ffffff" stroke-opacity="0.14" stroke-width="1.5" />
      <g transform="translate(9.4 9.4) scale(0.70625)" fill="#ffffff">
        <rect x="17" y="14" width="8" height="36" rx="2" />
        <rect x="39" y="14" width="8" height="36" rx="2" />
        <rect x="13.5" y="27" width="37" height="8" rx="4" />
      </g>
    </svg>
  );
}
