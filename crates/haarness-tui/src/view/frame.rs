//! Row-tagged frame assembly: every interactive row carries a `HitTarget`
//! so mouse clicks resolve through data, not coordinates guessed after the
//! fact. The frame records, per final screen row, what lives there.

use haarness_protocol::{ItemId, RequestId, ThreadId, WorkflowId};

/// What clicking a row does. Row-granular on purpose: this UI is built from
/// full-width rows, so per-row targets cover every affordance without a
/// pixel-level hit grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    /// Expand/collapse a tool result's output block, or a long prompt.
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
    /// Show or hide the sidebar, as ctrl+b does.
    SidebarToggle,
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

/// A rectangle drawn as a pane of its own over the frame: the sidebar, a
/// dialog card, a toast, the review viewer's file tree or patch. A drag that
/// starts in one copies only that pane's cells, and a drag that starts
/// beneath leaves it out. Rows and columns are terminal cells, end exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyPane {
    pub rows: std::ops::Range<usize>,
    pub columns: std::ops::Range<usize>,
}

impl CopyPane {
    fn contains(&self, row: usize, column: usize) -> bool {
        self.rows.contains(&row) && self.columns.contains(&column)
    }
}

/// One drawn character: the cell it starts at, the cells it takes, and
/// whether a selection copies it (see [`haarness_opentui::Span::selectable`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyCell {
    pub character: char,
    pub column: usize,
    pub width: usize,
    pub selectable: bool,
}

/// The last frame as a mouse selection reads it.
#[derive(Debug, Default)]
pub struct CopyFrame {
    pub rows: Vec<Vec<CopyCell>>,
    /// Back to front, as they were drawn.
    pub panes: Vec<CopyPane>,
}

impl CopyFrame {
    #[must_use]
    pub fn new(lines: &[haarness_opentui::Line], panes: Vec<CopyPane>) -> Self {
        use unicode_width::UnicodeWidthChar as _;
        let rows = lines
            .iter()
            .map(|line| {
                let mut column = 0;
                let mut cells = Vec::new();
                for span in &line.spans {
                    for character in span.text.chars() {
                        let width = character.width().unwrap_or(0);
                        cells.push(CopyCell {
                            character,
                            column,
                            width,
                            selectable: span.selectable,
                        });
                        column += width;
                    }
                }
                cells
            })
            .collect();
        Self { rows, panes }
    }

    /// The cells a drag from `anchor` to `head` (`(row, column)`) covers on
    /// each row: inside the pane the drag started in, without the panes
    /// drawn over it. Decoration is still included; see [`Self::text`].
    #[must_use]
    pub fn selected_columns(
        &self,
        anchor: (usize, usize),
        head: (usize, usize),
    ) -> Vec<(usize, Vec<std::ops::Range<usize>>)> {
        let ((first_row, first_column), (last_row, last_column)) = if anchor <= head {
            (anchor, head)
        } else {
            (head, anchor)
        };
        let pane = self
            .panes
            .iter()
            .rposition(|pane| pane.contains(anchor.0, anchor.1));
        let (rows, columns) = pane.map_or((0..self.rows.len(), 0..usize::MAX), |index| {
            (
                self.panes[index].rows.clone(),
                self.panes[index].columns.clone(),
            )
        });
        let above = &self.panes[pane.map_or(0, |index| index + 1)..];
        let mut selected = Vec::new();
        for row in first_row..=last_row.min(self.rows.len().saturating_sub(1)) {
            if !rows.contains(&row) {
                continue;
            }
            let from = if row == first_row {
                first_column.max(columns.start)
            } else {
                columns.start
            };
            let to = if row == last_row {
                (last_column + 1).min(columns.end)
            } else {
                columns.end
            };
            let mut ranges: Vec<std::ops::Range<usize>> = std::iter::once(from..to).collect();
            for cover in above.iter().filter(|cover| cover.rows.contains(&row)) {
                ranges = ranges
                    .into_iter()
                    .flat_map(|range| {
                        [
                            range.start..range.end.min(cover.columns.start),
                            range.start.max(cover.columns.end)..range.end,
                        ]
                    })
                    .filter(|range| !range.is_empty())
                    .collect();
            }
            ranges.retain(|range| !range.is_empty());
            selected.push((row, ranges));
        }
        selected
    }

    /// The selectable cells of [`Self::selected_columns`], as runs to paint.
    #[must_use]
    pub fn painted(
        &self,
        anchor: (usize, usize),
        head: (usize, usize),
    ) -> Vec<(usize, std::ops::Range<usize>)> {
        let mut runs = Vec::new();
        for (row, ranges) in self.selected_columns(anchor, head) {
            for range in ranges {
                let mut run: Option<std::ops::Range<usize>> = None;
                for cell in self.rows[row]
                    .iter()
                    .filter(|cell| cell.width > 0 && intersects(cell, &range))
                {
                    match (&mut run, cell.selectable) {
                        (Some(open), true) if open.end == cell.column => {
                            open.end = cell.column + cell.width;
                        }
                        (_, true) => {
                            runs.extend(run.take().map(|open| (row, open)));
                            run = Some(cell.column..cell.column + cell.width);
                        }
                        (_, false) => runs.extend(run.take().map(|open| (row, open))),
                    }
                }
                runs.extend(run.map(|open| (row, open)));
            }
        }
        runs
    }

    /// What a drag from `anchor` to `head` copies: the selected cells'
    /// characters without decoration, each row's end trimmed. A wide glyph
    /// comes whole from either of its cells, with the marks that combine
    /// with it.
    #[must_use]
    pub fn text(&self, anchor: (usize, usize), head: (usize, usize)) -> String {
        let mut out = Vec::new();
        for (row, ranges) in self.selected_columns(anchor, head) {
            let mut slice = String::new();
            for range in &ranges {
                let mut selected_base = false;
                for cell in &self.rows[row] {
                    let selected = if cell.width == 0 {
                        selected_base
                    } else {
                        intersects(cell, range)
                    };
                    if selected && cell.selectable {
                        slice.push(cell.character);
                    }
                    selected_base = selected;
                }
            }
            out.push(slice.trim_end().to_owned());
        }
        out.join("\n")
    }
}

fn intersects(cell: &CopyCell, range: &std::ops::Range<usize>) -> bool {
    cell.column + cell.width > range.start && cell.column < range.end
}
