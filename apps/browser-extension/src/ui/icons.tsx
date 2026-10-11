/*
 * The extension's functional icons: Lucide glyphs named by role, sized on
 * Desktop's five-step scale with the stroke in rendered pixels (the same rules
 * as apps/desktop/src/renderer/icons.tsx). Brand artwork lives in brand.tsx.
 */
import type { Component, JSX } from "solid-js";
import { Dynamic } from "solid-js/web";

import ArrowUpRight from "lucide-solid/icons/arrow-up-right";
import Check from "lucide-solid/icons/check";
import CircleAlert from "lucide-solid/icons/circle-alert";
import CircleCheck from "lucide-solid/icons/circle-check";
import Copy from "lucide-solid/icons/copy";
import EyeOff from "lucide-solid/icons/eye-off";
import Folder from "lucide-solid/icons/folder";
import Hand from "lucide-solid/icons/hand";
import Layers from "lucide-solid/icons/layers";
import LifeBuoy from "lucide-solid/icons/life-buoy";
import Pin from "lucide-solid/icons/pin";
import Play from "lucide-solid/icons/play";
import Puzzle from "lucide-solid/icons/puzzle";
import RefreshCw from "lucide-solid/icons/refresh-cw";
import Settings from "lucide-solid/icons/settings";
import ShieldCheck from "lucide-solid/icons/shield-check";
import Sparkles from "lucide-solid/icons/sparkles";
import Square from "lucide-solid/icons/square";
import SquareTerminal from "lucide-solid/icons/square-terminal";
import TriangleAlert from "lucide-solid/icons/triangle-alert";
import X from "lucide-solid/icons/x";

type Glyph = Component<{ size?: number; strokeWidth?: number; absoluteStrokeWidth?: boolean; fill?: string; class?: string }>;

const catalog = {
  external: ArrowUpRight,
  check: Check,
  done: CircleCheck,
  error: CircleAlert,
  alert: TriangleAlert,
  copy: Copy,
  hidden: EyeOff,
  project: Folder,
  pause: Hand,
  tabs: Layers,
  help: LifeBuoy,
  pin: Pin,
  resume: Play,
  extensions: Puzzle,
  refresh: RefreshCw,
  settings: Settings,
  safe: ShieldCheck,
  try: Sparkles,
  stop: Square,
  terminal: SquareTerminal,
  close: X,
} satisfies Record<string, Glyph>;

export type IconName = keyof typeof catalog;
export type IconSize = "xs" | "sm" | "md" | "lg" | "xl";

const scale: Record<IconSize, { box: number; stroke: number }> = {
  xs: { box: 12, stroke: 1.35 },
  sm: { box: 14, stroke: 1.45 },
  md: { box: 16, stroke: 1.5 },
  lg: { box: 18, stroke: 1.5 },
  xl: { box: 22, stroke: 1.6 },
};

export function Icon(props: { name: IconName; size?: IconSize; filled?: boolean; class?: string }): JSX.Element {
  const step = () => scale[props.size ?? "md"];
  return (
    <Dynamic
      component={catalog[props.name]}
      size={step().box}
      strokeWidth={step().stroke}
      absoluteStrokeWidth
      fill={props.filled ? "currentColor" : undefined}
      class={"icon" + (props.class ? " " + props.class : "")}
      aria-hidden="true"
    />
  );
}
