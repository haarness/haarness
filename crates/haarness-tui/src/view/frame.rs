//! Row-tagged frame assembly: every interactive row carries a `HitTarget`
//! so mouse clicks resolve through data, not coordinates guessed after the
//! fact. The frame records, per final screen row, what lives there.

use haarness_protocol::{ItemId, RequestId, ThreadId, WorkflowId};

/// What clicking a row does. Row-granular on purpose: this UI is built from
/// full-width rows, so per-row targets cover every affordance without a
/// pixel-level hit grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    /// Expand/collapse a tool result's output block.
    ToolToggle(ItemId),
    /// Open a child agent's live thread view (task rows).
    TaskOpen(String),
    /// Open the durable workflow run referenced by a workflow tool row.
    WorkflowOpen(WorkflowId),
    /// Return from a child-agent thread to its parent conversation.
    ChildParent,
    /// Move to the previous child-agent thread in the current sibling set.
    ChildPrevious,
    /// Move to the next child-agent thread in the current sibling set.
    ChildNext,
    /// Select + activate a row in the active selector/suggestion menu.
    MenuRow(usize),
    /// Open a thread (home-screen recent list, sidebar).
    ThreadOpen(ThreadId),
    /// Focus the pending permission prompt (⚠ rows, accent status line).
    PermissionFocus,
    /// Activate one option chip on the docked approval panel, by index into
    /// the currently rendered chip row (answer choices, or confirm/cancel
    /// while an "always" answer awaits its confirmation).
    PermissionOption(usize),
    /// Collapse/expand a sidebar section by index.
    SidebarSection(usize),
    /// Focus/toggle a choice on the active single- or multi-select question.
    QuestionChoice(usize),
    /// Non-actionable cells inside a modal card; prevents click-through while
    /// letting a click outside the card dismiss the dialog.
    DialogSurface,
    /// One logical read-only item in an informational dialog. Every rendered
    /// row in the item (label, metadata, and wrapped description) carries the
    /// same index so hover feedback paints the block as a unit.
    DialogItem(usize),
    /// Dismiss the transient top-right toast.
    ToastDismiss,
    /// A row of the review viewer's file tree, by flattened index.
    DiffTreeRow(usize),
    /// A row of the review viewer's patch pane, by display index.
    DiffBodyRow(usize),
    /// The queue strip's header or `+N more` row: open every queued message.
    QueueList,
    /// One queued message in the strip: open its actions. Keyed by id so a
    /// queue that changed under the pointer cannot act on its neighbour.
    QueueItem(RequestId),
    /// The strip's editing line: cancel the edit and restore the draft.
    QueueEditCancel,
}

/// One clickable horizontal region in the final terminal frame. `start` is
/// inclusive and `end` exclusive, in terminal cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitRegion {
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub target: HitTarget,
    /// Paint focus feedback on pointer hover. Transcript targets stay
    /// clickable without turning every tool/output row into a bright bar.
    pub paint_hover: bool,
}

/// Lines plus row-relative tags (`index` into `lines`).
#[derive(Default)]
pub struct TaggedLines {
    pub lines: Vec<haarness_opentui::Line>,
    /// Stable message identities and their first rendered row, before windowing.
    pub messages: Vec<(ItemId, usize)>,
    pub tags: Vec<(usize, HitTarget)>,
    /// Column-bounded targets: `(row, start, end, target)` in terminal cells.
    /// Rows stay the interaction unit everywhere else; this exists for chip
    /// rows that pack several buttons into one line.
    pub ranged: Vec<(usize, usize, usize, HitTarget)>,
}

impl TaggedLines {
    pub fn push(&mut self, line: haarness_opentui::Line) {
        self.lines.push(line);
    }
    pub fn push_tagged(&mut self, line: haarness_opentui::Line, target: HitTarget) {
        self.tags.push((self.lines.len(), target));
        self.lines.push(line);
    }
    /// Push one line carrying several column-bounded targets.
    pub fn push_ranged(
        &mut self,
        line: haarness_opentui::Line,
        targets: impl IntoIterator<Item = (usize, usize, HitTarget)>,
    ) {
        let row = self.lines.len();
        self.ranged.extend(
            targets
                .into_iter()
                .map(|(from, to, tag)| (row, from, to, tag)),
        );
        self.lines.push(line);
    }
    pub fn extend(&mut self, lines: impl IntoIterator<Item = haarness_opentui::Line>) {
        self.lines.extend(lines);
    }
    /// Append another tagged block, re-basing its tags.
    #[allow(dead_code)] // used as the composition seam by upcoming views
    pub fn append(&mut self, mut other: TaggedLines) {
        let base = self.lines.len();
        self.lines.append(&mut other.lines);
        self.messages
            .extend(other.messages.into_iter().map(|(id, row)| (id, base + row)));
        self.tags
            .extend(other.tags.into_iter().map(|(row, tag)| (base + row, tag)));
        self.ranged.extend(
            other
                .ranged
                .into_iter()
                .map(|(row, from, to, tag)| (base + row, from, to, tag)),
        );
    }
    pub fn len(&self) -> usize {
        self.lines.len()
    }
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}
