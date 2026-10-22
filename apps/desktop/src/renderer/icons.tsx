/*
 * The functional icon vocabulary.
 *
 * Every functional glyph in the renderer comes from Lucide through this file.
 * Call sites name a *role* ("review", "worktree", "stop") rather than a
 * drawing, so the artwork can change in one place and so two features that mean
 * the same thing cannot drift into two different pictures.
 *
 * Sizes are a five-step scale, never arbitrary pixels, and the stroke is
 * specified in rendered pixels (`absoluteStrokeWidth`) so a 12px icon and a
 * 22px icon have the same optical weight next to text.
 *
 * Haarness's own brand artwork — the Haarness. wordmark and the H mark — lives
 * in brand.tsx and deliberately stays out of this catalogue. Provider brand
 * artwork lives in ProviderIcon.tsx and the provider-brands registry.
 */
import type { Component, JSX } from "solid-js";
import { Dynamic } from "solid-js/web";

import Archive from "lucide-solid/icons/archive";
import ArchiveRestore from "lucide-solid/icons/archive-restore";
import ArrowDown from "lucide-solid/icons/arrow-down";
import ArrowLeft from "lucide-solid/icons/arrow-left";
import ArrowRight from "lucide-solid/icons/arrow-right";
import ArrowUp from "lucide-solid/icons/arrow-up";
import ArrowUpRight from "lucide-solid/icons/arrow-up-right";
import BookOpen from "lucide-solid/icons/book-open";
import Bot from "lucide-solid/icons/bot";
import Check from "lucide-solid/icons/check";
import ChevronDown from "lucide-solid/icons/chevron-down";
import ChevronUp from "lucide-solid/icons/chevron-up";
import ChevronRight from "lucide-solid/icons/chevron-right";
import ChartColumn from "lucide-solid/icons/chart-column";
import CreditCard from "lucide-solid/icons/credit-card";
import Laptop from "lucide-solid/icons/laptop";
import LogOut from "lucide-solid/icons/log-out";
import Play from "lucide-solid/icons/play";
import Server from "lucide-solid/icons/server";
import CircleAlert from "lucide-solid/icons/circle-alert";
import CircleCheck from "lucide-solid/icons/circle-check";
import CircleDashed from "lucide-solid/icons/circle-dashed";
import CircleMinus from "lucide-solid/icons/circle-minus";
import CircleHelp from "lucide-solid/icons/circle-help";
import Cloud from "lucide-solid/icons/cloud";
import Code from "lucide-solid/icons/code";
import Copy from "lucide-solid/icons/copy";
import Download from "lucide-solid/icons/download";
import Ellipsis from "lucide-solid/icons/ellipsis";
import Eye from "lucide-solid/icons/eye";
import EyeOff from "lucide-solid/icons/eye-off";
import File from "lucide-solid/icons/file";
import FileText from "lucide-solid/icons/file-text";
import Filter from "lucide-solid/icons/filter";
import Folder from "lucide-solid/icons/folder";
import FolderOpen from "lucide-solid/icons/folder-open";
import FolderPlus from "lucide-solid/icons/folder-plus";
import FilePlus from "lucide-solid/icons/file-plus";
import GitBranch from "lucide-solid/icons/git-branch";
import GitCompare from "lucide-solid/icons/git-compare";
import Globe from "lucide-solid/icons/globe";
import KeyRound from "lucide-solid/icons/key-round";
import Layers from "lucide-solid/icons/layers";
import Link from "lucide-solid/icons/link-2";
import List from "lucide-solid/icons/list";
import ListTodo from "lucide-solid/icons/list-todo";
import Maximize from "lucide-solid/icons/maximize-2";
import MessageCircle from "lucide-solid/icons/message-circle";
import MessageSquare from "lucide-solid/icons/message-square";
import MonitorSmartphone from "lucide-solid/icons/monitor-smartphone";
import Mic from "lucide-solid/icons/mic";
import Minimize from "lucide-solid/icons/minimize-2";
import PanelLeft from "lucide-solid/icons/panel-left";
import PanelRight from "lucide-solid/icons/panel-right";
import Paperclip from "lucide-solid/icons/paperclip";
import ImageIcon from "lucide-solid/icons/image";
import Pencil from "lucide-solid/icons/pencil";
import Pin from "lucide-solid/icons/pin";
import PinOff from "lucide-solid/icons/pin-off";
import Plus from "lucide-solid/icons/plus";
import Puzzle from "lucide-solid/icons/puzzle";
import RefreshCw from "lucide-solid/icons/refresh-cw";
import Search from "lucide-solid/icons/search";
import Settings from "lucide-solid/icons/settings";
import Shield from "lucide-solid/icons/shield";
import Split from "lucide-solid/icons/split";
import Square from "lucide-solid/icons/square";
import SquarePen from "lucide-solid/icons/square-pen";
import SquareTerminal from "lucide-solid/icons/square-terminal";
import Timer from "lucide-solid/icons/timer";
import Trash from "lucide-solid/icons/trash-2";
import TriangleAlert from "lucide-solid/icons/triangle-alert";
import Wrench from "lucide-solid/icons/wrench";
import X from "lucide-solid/icons/x";
import Zap from "lucide-solid/icons/zap";
import Undo from "lucide-solid/icons/undo-2";
import Redo from "lucide-solid/icons/redo-2";

type Glyph = Component<{
  size?: number;
  strokeWidth?: number;
  absoluteStrokeWidth?: boolean;
  fill?: string;
  class?: string;
}>;

/**
 * Role → glyph. Names describe what the control does in Haarness; several roles
 * may deliberately share one drawing when they mean the same thing to a reader.
 */
const catalog = {
  // Navigation and window chrome
  sidebar: PanelLeft,
  panel: PanelRight,
  expand: Maximize,
  shrink: Minimize,
  search: Search,
  settings: Settings,
  close: X,
  more: Ellipsis,
  chevron: ChevronRight,
  down: ChevronDown,
  up: ArrowUp,
  // Reordering in a list, kept apart from `up`/`send`, which are arrows.
  "move-up": ChevronUp,
  "move-down": ChevronDown,
  check: Check,
  plus: Plus,

  // Projects, conversations and history
  compose: SquarePen,
  chat: MessageSquare,
  "side-chat": MessageCircle,
  folder: Folder,
  "folder-open": FolderOpen,
  "folder-new": FolderPlus,
  "file-new": FilePlus,
  branch: GitBranch,
  fork: Split,
  pin: Pin,
  unpin: PinOff,
  section: List,
  archive: Archive,
  restore: ArchiveRestore,
  undo: Undo,
  redo: Redo,
  clock: Timer,

  // Workspace tools
  file: File,
  "file-text": FileText,
  diff: GitCompare,
  terminal: SquareTerminal,
  edit: Pencil,
  code: Code,
  eye: Eye,
  "eye-off": EyeOff,
  filter: Filter,
  trash: Trash,
  download: Download,
  refresh: RefreshCw,
  copy: Copy,
  attach: Paperclip,
  image: ImageIcon,

  // Engine capabilities
  agents: Bot,
  plugin: Puzzle,
  tools: Wrench,
  chart: ChartColumn,
  layers: Layers,
  link: Link,
  book: BookOpen,
  cloud: Cloud,
  reasoning: Zap,
  web: Globe,
  key: KeyRound,
  todo: ListTodo,
  "todo-open": CircleDashed,
  "todo-done": CircleCheck,
  "todo-cancelled": CircleMinus,

  // Turn state
  send: ArrowUp,
  stop: Square,
  shield: Shield,
  mic: Mic,
  jump: ArrowDown,
  alert: TriangleAlert,
  error: CircleAlert,
  question: CircleHelp,
  external: ArrowUpRight,
  back: ArrowLeft,
  forward: ArrowRight,
  "sign-out": LogOut,
  start: Play,
  billing: CreditCard,
  computer: Laptop,
  server: Server,
  remote: MonitorSmartphone,
} satisfies Record<string, Glyph>;

export type IconName = keyof typeof catalog;
export type IconSize = "xs" | "sm" | "md" | "lg" | "xl";

/** Rendered box, and the stroke width in *rendered* pixels for that box. */
const scale: Record<IconSize, { box: number; stroke: number }> = {
  xs: { box: 12, stroke: 1.35 },
  sm: { box: 14, stroke: 1.45 },
  md: { box: 16, stroke: 1.5 },
  lg: { box: 18, stroke: 1.5 },
  xl: { box: 22, stroke: 1.6 },
};

export interface IconProps {
  name: IconName;
  size?: IconSize;
  /** Only for glyphs that read as solid shapes, such as the Stop square. */
  filled?: boolean;
  class?: string;
}

export function Icon(props: IconProps): JSX.Element {
  const step = () => scale[props.size ?? "md"];
  return (
    <Dynamic
      component={catalog[props.name] ?? catalog.file}
      size={step().box}
      strokeWidth={step().stroke}
      absoluteStrokeWidth
      fill={props.filled ? "currentColor" : undefined}
      class={"icon" + (props.class ? " " + props.class : "")}
    />
  );
}
