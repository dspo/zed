//! IntelliJ-style Three-Way Merge Editor
//!
//! This provides a true three-panel merge editor:
//! - Left panel: "Theirs" version (incoming branch, read-only)
//! - Center panel: "Base" version (common ancestor, read-only or editable in Resolve mode)
//! - Right panel: "Ours" version (current branch, read-only)
//!
//! Features:
//! - Synchronized scrolling across all three panels
//! - Hunk alignment with padding blocks
//! - Two modes: Read View (all read-only) and Resolve View (base editable)
//! - Prev/Next Diff navigation
//! - Accept/Ignore actions in gutters between panels
//! - Mark As Resolved when all conflicts are handled

use anyhow::Result;
use editor::{
    Editor, EditorEvent, ExcerptRange, RowHighlightOptions, ToPoint,
    display_map::{BlockContext, BlockPlacement, BlockProperties, BlockStyle, CustomBlockId},
    scroll::Autoscroll,
};
use gpui::{
    AnyElement, App, AppContext as _, Context, DragMoveEvent, Entity, EventEmitter, 
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, 
    Render, Styled, Subscription, Task, Window, actions, div, 
    relative, px,
};
use language::{Buffer, Capability, Point};
use multi_buffer::MultiBuffer;
use project::{ConflictRegion, Project, ProjectPath};
use similar::TextDiff;
use std::{
    any::Any,
    cell::Cell,
    path::PathBuf,
    sync::Arc,
};
use ui::{
    ActiveTheme, Color, Icon, IconButton, IconName, Label, LabelCommon as _, LabelSize,
    SharedString, Tooltip, prelude::*,
};
use workspace::{
    Item, ItemNavHistory, Workspace,
    item::{ItemEvent, TabContentParams},
};

// Actions for navigation in three-way merge editor
actions!(
    three_way_merge,
    [
        GoToNextDiff,
        GoToPreviousDiff,
        ToggleResolveMode,
        MarkAsResolved,
    ]
);

/// Register keybindings for three-way merge editor
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("alt-]", GoToNextDiff, Some("ThreeWayMergeEditor")),
        KeyBinding::new("alt-[", GoToPreviousDiff, Some("ThreeWayMergeEditor")),
    ]);
}

/// Width of the divider area that contains hunk buttons
const DIVIDER_WIDTH: gpui::Pixels = gpui::px(36.);

/// Information about a visible hunk for rendering buttons
#[derive(Clone)]
struct VisibleHunk {
    /// Index in the hunks vector
    index: usize,
    /// Vertical offset from top of the divider area (in pixels)
    top_offset: f32,
    /// Height of this hunk region (in pixels)
    height: f32,
    /// Whether this hunk is pending (needs action)
    is_pending: bool,
    /// The row in source editor where this hunk starts
    source_start_row: u32,
    /// The row in base editor where this hunk maps to
    base_start_row: u32,
}

/// Marker for dragging the left divider (between theirs and base)
#[derive(Clone)]
struct DraggedLeftDivider;

impl Render for DraggedLeftDivider {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Marker for dragging the right divider (between base and ours)
#[derive(Clone)]
struct DraggedRightDivider;

impl Render for DraggedRightDivider {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Marker types for row highlighting
struct TheirsHighlight;
struct BaseHighlight;
struct OursHighlight;

/// Status of a hunk in the merge process
#[derive(Clone, Copy, PartialEq, Debug)]
enum HunkStatus {
    /// Hunk has not been processed yet
    Pending,
    /// Hunk was accepted (applied to base)
    Accepted,
    /// Hunk was ignored (not applied)
    Ignored,
}

/// Type of change in a diff hunk
#[derive(Clone, Copy, PartialEq, Debug)]
enum DiffChangeKind {
    /// Lines added in this version compared to base
    Added,
    /// Lines deleted in this version compared to base  
    Deleted,
    /// Lines modified (deleted from base and added new)
    Modified,
}

/// Information about a diff hunk for merge
#[derive(Clone, Debug)]
struct MergeHunk {
    /// Which side this hunk is from (Theirs or Ours)
    side: MergeSide,
    /// Type of change
    kind: DiffChangeKind,
    /// Row range in the source editor (theirs or ours) - 0-indexed
    source_rows: std::ops::Range<u32>,
    /// Row range in the base editor where this applies - 0-indexed
    base_rows: std::ops::Range<u32>,
    /// The text content of this hunk (for additions/modifications)
    text: String,
    /// Current status of this hunk
    status: HunkStatus,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MergeSide {
    Theirs,
    Ours,
}

/// Target for padding blocks
#[derive(Clone, Copy, Debug)]
enum PaddingTarget {
    Theirs,
    Base,
    Ours,
}

/// Three-way merge editor for resolving conflicts (IntelliJ style)
#[allow(dead_code)]
pub struct ThreeWayMergeEditor {
    /// Left panel: "Theirs" (incoming changes, read-only)
    theirs_editor: Entity<Editor>,
    /// Center panel: "Base" (common ancestor, editable in Resolve mode)
    base_editor: Entity<Editor>,
    /// Right panel: "Ours" (current branch, read-only)
    ours_editor: Entity<Editor>,
    
    /// The theirs buffer
    theirs_buffer: Entity<Buffer>,
    /// The base buffer (original conflict file content for editing)
    base_buffer: Entity<Buffer>,
    /// The ours buffer
    ours_buffer: Entity<Buffer>,
    
    /// The original conflict region info
    conflict: ConflictRegion,
    /// Path of the conflicting file
    path: PathBuf,
    
    /// Whether we're in Resolve mode (base editable) or Read mode (all read-only)
    is_resolve_mode: bool,
    
    /// Tracked hunks from theirs side with their status
    theirs_hunks: Vec<MergeHunk>,
    /// Tracked hunks from ours side with their status
    ours_hunks: Vec<MergeHunk>,
    
    /// Panel width ratios (theirs, base, ours)
    /// theirs_ratio + base_ratio + ours_ratio = 1.0
    theirs_ratio: f32,
    ours_ratio: f32,
    
    /// Focus handle
    focus_handle: FocusHandle,
    /// Prevent recursive scroll sync
    is_syncing_scroll: Cell<bool>,
    
    /// Alignment blocks inserted in each editor
    theirs_alignment_blocks: Vec<CustomBlockId>,
    base_alignment_blocks: Vec<CustomBlockId>,
    ours_alignment_blocks: Vec<CustomBlockId>,
    
    /// Subscriptions for event handling
    _subscriptions: Vec<Subscription>,
}

impl ThreeWayMergeEditor {
    /// Create a new three-way merge editor
    pub fn new(
        theirs_text: String,
        base_text: String,
        ours_text: String,
        conflict: ConflictRegion,
        path: PathBuf,
        project: Option<Entity<Project>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // Create "Theirs" buffer and editor (left, read-only)
        let theirs_buffer = cx.new(|cx| Buffer::local(theirs_text.clone(), cx));
        let theirs_multibuffer = cx.new(|cx| {
            let mut mb = MultiBuffer::without_headers(Capability::ReadOnly);
            mb.push_excerpts(
                theirs_buffer.clone(),
                [ExcerptRange::new(text::Anchor::MIN..text::Anchor::MAX)],
                cx,
            );
            mb
        });
        let theirs_editor = cx.new(|cx| {
            let mut editor = Editor::for_multibuffer(
                theirs_multibuffer.clone(),
                project.clone(),
                window,
                cx,
            );
            editor.set_read_only(true);
            editor.set_show_gutter(true, cx);
            editor
        });

        // Create "Base" buffer and editor (center, initially read-only)
        let base_buffer = cx.new(|cx| Buffer::local(base_text.clone(), cx));
        let base_multibuffer = cx.new(|cx| {
            let mut mb = MultiBuffer::without_headers(Capability::ReadWrite);
            mb.push_excerpts(
                base_buffer.clone(),
                [ExcerptRange::new(text::Anchor::MIN..text::Anchor::MAX)],
                cx,
            );
            mb
        });
        let base_editor = cx.new(|cx| {
            let mut editor = Editor::for_multibuffer(
                base_multibuffer.clone(),
                project.clone(),
                window,
                cx,
            );
            editor.set_read_only(true); // Start in Read mode
            editor.set_show_gutter(true, cx);
            editor
        });

        // Create "Ours" buffer and editor (right, read-only)
        let ours_buffer = cx.new(|cx| Buffer::local(ours_text.clone(), cx));
        let ours_multibuffer = cx.new(|cx| {
            let mut mb = MultiBuffer::without_headers(Capability::ReadOnly);
            mb.push_excerpts(
                ours_buffer.clone(),
                [ExcerptRange::new(text::Anchor::MIN..text::Anchor::MAX)],
                cx,
            );
            mb
        });
        let ours_editor = cx.new(|cx| {
            let mut editor = Editor::for_multibuffer(
                ours_multibuffer.clone(),
                project.clone(),
                window,
                cx,
            );
            editor.set_read_only(true);
            editor.set_show_gutter(true, cx);
            editor
        });

        // Set up scroll synchronization between all three editors
        let mut subscriptions = Vec::new();

        // Theirs -> sync others
        let base_for_theirs = base_editor.clone();
        let ours_for_theirs = ours_editor.clone();
        subscriptions.push(cx.subscribe_in(
            &theirs_editor,
            window,
            move |this, _, event: &EditorEvent, window, cx| {
                if let EditorEvent::ScrollPositionChanged { local: true, .. } = event {
                    if !this.is_syncing_scroll.get() {
                        this.is_syncing_scroll.set(true);
                        let pos = this.theirs_editor.update(cx, |e, cx| e.scroll_position(cx));
                        base_for_theirs.update(cx, |e, cx| e.set_scroll_position(pos, window, cx));
                        ours_for_theirs.update(cx, |e, cx| e.set_scroll_position(pos, window, cx));
                        this.is_syncing_scroll.set(false);
                    }
                }
            },
        ));

        // Base -> sync others
        let theirs_for_base = theirs_editor.clone();
        let ours_for_base = ours_editor.clone();
        subscriptions.push(cx.subscribe_in(
            &base_editor,
            window,
            move |this, _, event: &EditorEvent, window, cx| {
                if let EditorEvent::ScrollPositionChanged { local: true, .. } = event {
                    if !this.is_syncing_scroll.get() {
                        this.is_syncing_scroll.set(true);
                        let pos = this.base_editor.update(cx, |e, cx| e.scroll_position(cx));
                        theirs_for_base.update(cx, |e, cx| e.set_scroll_position(pos, window, cx));
                        ours_for_base.update(cx, |e, cx| e.set_scroll_position(pos, window, cx));
                        this.is_syncing_scroll.set(false);
                    }
                }
            },
        ));

        // Ours -> sync others
        let theirs_for_ours = theirs_editor.clone();
        let base_for_ours = base_editor.clone();
        subscriptions.push(cx.subscribe_in(
            &ours_editor,
            window,
            move |this, _, event: &EditorEvent, window, cx| {
                if let EditorEvent::ScrollPositionChanged { local: true, .. } = event {
                    if !this.is_syncing_scroll.get() {
                        this.is_syncing_scroll.set(true);
                        let pos = this.ours_editor.update(cx, |e, cx| e.scroll_position(cx));
                        theirs_for_ours.update(cx, |e, cx| e.set_scroll_position(pos, window, cx));
                        base_for_ours.update(cx, |e, cx| e.set_scroll_position(pos, window, cx));
                        this.is_syncing_scroll.set(false);
                    }
                }
            },
        ));

        let mut view = Self {
            theirs_editor,
            base_editor,
            ours_editor,
            theirs_buffer,
            base_buffer,
            ours_buffer,
            conflict,
            path,
            is_resolve_mode: false,
            theirs_hunks: Vec::new(),
            ours_hunks: Vec::new(),
            theirs_ratio: 1.0 / 3.0,
            ours_ratio: 1.0 / 3.0,
            focus_handle,
            is_syncing_scroll: Cell::new(false),
            theirs_alignment_blocks: Vec::new(),
            base_alignment_blocks: Vec::new(),
            ours_alignment_blocks: Vec::new(),
            _subscriptions: subscriptions,
        };

        // Calculate initial alignment and highlighting
        view.update_alignment_and_highlighting(window, cx);

        view
    }

    /// Open a three-way merge editor for a conflicted file in the workspace
    pub fn open(
        conflict: ConflictRegion,
        result_buffer: Entity<Buffer>,
        path: PathBuf,
        project: Entity<Project>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        // Extract text from conflict region or use stored stage texts
        let result_snapshot = result_buffer.read(cx).snapshot();
        
        let theirs_text = conflict
            .theirs_text
            .clone()
            .unwrap_or_else(|| {
                result_snapshot
                    .text_for_range(conflict.theirs.clone())
                    .collect()
            });
        
        let ours_text = conflict
            .ours_text
            .clone()
            .unwrap_or_else(|| {
                result_snapshot
                    .text_for_range(conflict.ours.clone())
                    .collect()
            });
        
        // Use base text if available, otherwise use empty string
        let base_text = conflict.base_text.clone().unwrap_or_default();
        
        let view = cx.new(|cx| {
            Self::new(
                theirs_text,
                base_text,
                ours_text,
                conflict,
                path,
                Some(project),
                window,
                cx,
            )
        });
        
        workspace.add_item_to_active_pane(
            Box::new(view),
            None,
            true,
            window,
            cx,
        );
    }

    /// Toggle between Read View and Resolve View modes
    fn toggle_resolve_mode(&mut self, _: &ToggleResolveMode, _window: &mut Window, cx: &mut Context<Self>) {
        self.is_resolve_mode = !self.is_resolve_mode;
        
        // Update base editor editability
        self.base_editor.update(cx, |editor, _cx| {
            editor.set_read_only(!self.is_resolve_mode);
        });
        
        cx.notify();
    }

    /// Navigate to the next diff hunk
    fn go_to_next_diff(&mut self, _: &GoToNextDiff, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_to_diff(true, window, cx);
    }

    /// Navigate to the previous diff hunk
    fn go_to_previous_diff(&mut self, _: &GoToPreviousDiff, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_to_diff(false, window, cx);
    }

    /// Navigate to the next or previous diff hunk
    fn navigate_to_diff(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Get current cursor position in base editor
        let current_row = self.base_editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let selection = editor.selections.newest::<Point>(&snapshot);
            selection.head().row
        });

        // Collect all hunk start rows from both sides
        let mut hunk_rows: Vec<u32> = Vec::new();
        for hunk in &self.theirs_hunks {
            if hunk.status == HunkStatus::Pending {
                hunk_rows.push(hunk.base_rows.start);
            }
        }
        for hunk in &self.ours_hunks {
            if hunk.status == HunkStatus::Pending {
                hunk_rows.push(hunk.base_rows.start);
            }
        }
        hunk_rows.sort();
        hunk_rows.dedup();

        if hunk_rows.is_empty() {
            return;
        }

        // Find target hunk
        let target_row = if next {
            hunk_rows.iter().find(|&&row| row > current_row)
                .or_else(|| hunk_rows.first())
                .copied()
        } else {
            hunk_rows.iter().rev().find(|&&row| row < current_row)
                .or_else(|| hunk_rows.last())
                .copied()
        };

        let Some(target_row) = target_row else {
            return;
        };

        // Navigate base editor to target
        self.base_editor.update(cx, |editor, cx| {
            let destination = Point::new(target_row, 0);
            editor.unfold_ranges(&[destination..destination], false, false, cx);
            editor.change_selections(
                editor::SelectionEffects::scroll(Autoscroll::top_relative(5)),
                window,
                cx,
                |s| s.select_ranges([destination..destination]),
            );
        });

        // Focus base editor
        self.base_editor.update(cx, |_editor, cx| {
            cx.focus_self(window);
        });

        // Sync scroll to other editors
        let theirs_editor = self.theirs_editor.clone();
        let ours_editor = self.ours_editor.clone();
        let base_editor = self.base_editor.clone();
        window.defer(cx, move |window, cx| {
            let pos = base_editor.update(cx, |e, cx| e.scroll_position(cx));
            theirs_editor.update(cx, |e, cx| e.set_scroll_position(pos, window, cx));
            ours_editor.update(cx, |e, cx| e.set_scroll_position(pos, window, cx));
        });
    }

    /// Check if all hunks have been processed (accepted or ignored)
    fn all_hunks_processed(&self) -> bool {
        self.theirs_hunks.iter().all(|h| h.status != HunkStatus::Pending)
            && self.ours_hunks.iter().all(|h| h.status != HunkStatus::Pending)
    }

    /// Get navigation state (has_prev, has_next)
    fn diff_navigation_state(&self, cx: &App) -> (bool, bool) {
        let base_editor = self.base_editor.read(cx);
        let mb_snapshot = base_editor.buffer().read(cx).snapshot(cx);
        let current_row = base_editor.selections.newest_anchor()
            .head()
            .to_point(&mb_snapshot)
            .row;

        let pending_rows: Vec<u32> = self.theirs_hunks.iter()
            .chain(self.ours_hunks.iter())
            .filter(|h| h.status == HunkStatus::Pending)
            .map(|h| h.base_rows.start)
            .collect();

        let has_prev = pending_rows.iter().any(|&row| row < current_row);
        let has_next = pending_rows.iter().any(|&row| row > current_row);

        (has_prev, has_next)
    }

    /// Get the count of pending diffs
    fn pending_diff_count(&self) -> usize {
        self.theirs_hunks.iter().filter(|h| h.status == HunkStatus::Pending).count()
            + self.ours_hunks.iter().filter(|h| h.status == HunkStatus::Pending).count()
    }

    /// Get visible hunks for theirs side with their pixel positions
    fn get_visible_theirs_hunks(&self, line_height: f32, scroll_y: f32) -> Vec<VisibleHunk> {
        // Use a large viewport estimate; actual clipping will handle visibility
        let viewport_lines: u32 = 100;
        
        self.theirs_hunks.iter().enumerate()
            .filter_map(|(index, hunk)| {
                let source_start = hunk.source_rows.start as f32;
                let source_end = hunk.source_rows.end.max(hunk.source_rows.start + 1) as f32;
                
                // Check if hunk is within visible range
                let scroll_start = scroll_y;
                let scroll_end = scroll_y + viewport_lines as f32;
                
                if source_end < scroll_start || source_start > scroll_end {
                    return None; // Not visible
                }
                
                let top_offset = (source_start - scroll_y) * line_height;
                let height = (source_end - source_start) * line_height;
                
                Some(VisibleHunk {
                    index,
                    top_offset,
                    height,
                    is_pending: hunk.status == HunkStatus::Pending,
                    source_start_row: hunk.source_rows.start,
                    base_start_row: hunk.base_rows.start,
                })
            })
            .collect()
    }

    /// Get visible hunks for ours side with their pixel positions
    fn get_visible_ours_hunks(&self, line_height: f32, scroll_y: f32) -> Vec<VisibleHunk> {
        let viewport_lines: u32 = 100;
        
        self.ours_hunks.iter().enumerate()
            .filter_map(|(index, hunk)| {
                let source_start = hunk.source_rows.start as f32;
                let source_end = hunk.source_rows.end.max(hunk.source_rows.start + 1) as f32;
                
                // Check if hunk is within visible range
                let scroll_start = scroll_y;
                let scroll_end = scroll_y + viewport_lines as f32;
                
                if source_end < scroll_start || source_start > scroll_end {
                    return None; // Not visible
                }
                
                let top_offset = (source_start - scroll_y) * line_height;
                let height = (source_end - source_start) * line_height;
                
                Some(VisibleHunk {
                    index,
                    top_offset,
                    height,
                    is_pending: hunk.status == HunkStatus::Pending,
                    source_start_row: hunk.source_rows.start,
                    base_start_row: hunk.base_rows.start,
                })
            })
            .collect()
    }

    /// Update alignment blocks and highlighting using real diff calculation
    fn update_alignment_and_highlighting(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // Clear existing highlights and hunks
        self.clear_alignment_blocks(cx);

        // Get text from all three buffers
        let base_text = self.base_buffer.read(cx).text();
        let theirs_text = self.theirs_buffer.read(cx).text();
        let ours_text = self.ours_buffer.read(cx).text();

        // Compute diffs: base vs theirs and base vs ours
        let theirs_hunks = self.compute_diff_hunks(&base_text, &theirs_text, MergeSide::Theirs);
        let ours_hunks = self.compute_diff_hunks(&base_text, &ours_text, MergeSide::Ours);

        // Store hunks
        self.theirs_hunks = theirs_hunks;
        self.ours_hunks = ours_hunks;

        // Theme colors for highlighting
        let theirs_addition_color = cx.theme().colors().version_control_conflict_marker_theirs.opacity(0.20);
        let ours_addition_color = cx.theme().colors().version_control_conflict_marker_ours.opacity(0.20);
        
        let highlight_options = RowHighlightOptions {
            include_gutter: true,
            ..Default::default()
        };


        #[derive(Clone, Debug)]
        struct ChangeEvent {
            base_range: std::ops::Range<u32>,
            theirs_range: Option<std::ops::Range<u32>>, // source rows in theirs
            ours_range: Option<std::ops::Range<u32>>,   // source rows in ours
        }

        // Collect all change events from both sides
        let mut events: Vec<ChangeEvent> = Vec::new();

        for hunk in &self.theirs_hunks {
            if hunk.status != HunkStatus::Pending {
                continue;
            }
            events.push(ChangeEvent {
                base_range: hunk.base_rows.clone(),
                theirs_range: Some(hunk.source_rows.clone()),
                ours_range: None,
            });
        }

        for hunk in &self.ours_hunks {
            if hunk.status != HunkStatus::Pending {
                continue;
            }
            events.push(ChangeEvent {
                base_range: hunk.base_rows.clone(),
                theirs_range: None,
                ours_range: Some(hunk.source_rows.clone()),
            });
        }

        // Sort by base_range.start, then by base_range.end
        events.sort_by(|a, b| {
            a.base_range.start.cmp(&b.base_range.start)
                .then(a.base_range.end.cmp(&b.base_range.end))
        });

        // Merge overlapping/adjacent events into unified regions
        #[derive(Clone, Debug)]
        struct MergedRegion {
            base_start: u32,
            base_end: u32,
            theirs_ranges: Vec<std::ops::Range<u32>>,
            ours_ranges: Vec<std::ops::Range<u32>>,
        }

        let mut merged_regions: Vec<MergedRegion> = Vec::new();

        for event in events {
            // Check if this event overlaps with the last merged region
            let should_merge = merged_regions.last().map_or(false, |last| {
                // Overlapping or adjacent in base
                event.base_range.start <= last.base_end
            });

            if should_merge {
                let last = merged_regions.last_mut().unwrap();
                last.base_end = last.base_end.max(event.base_range.end);
                if let Some(r) = event.theirs_range {
                    last.theirs_ranges.push(r);
                }
                if let Some(r) = event.ours_range {
                    last.ours_ranges.push(r);
                }
            } else {
                merged_regions.push(MergedRegion {
                    base_start: event.base_range.start,
                    base_end: event.base_range.end,
                    theirs_ranges: event.theirs_range.into_iter().collect(),
                    ours_ranges: event.ours_range.into_iter().collect(),
                });
            }
        }

        // Now compute padding for each merged region
        // Key insight: We need to track the cumulative offset between base and each side
        // because previous changes affect where subsequent padding should be inserted.
        
        // Build a mapping: for each base row position, what's the corresponding row in theirs/ours
        // This is done by tracking how many lines were added/removed before each point
        
        // First, collect all theirs hunks sorted by base position
        let mut theirs_changes: Vec<(u32, i32)> = Vec::new(); // (base_row, delta)
        for hunk in &self.theirs_hunks {
            if hunk.status != HunkStatus::Pending {
                continue;
            }
            let base_lines = (hunk.base_rows.end - hunk.base_rows.start) as i32;
            let theirs_lines = (hunk.source_rows.end - hunk.source_rows.start) as i32;
            let delta = theirs_lines - base_lines;
            theirs_changes.push((hunk.base_rows.start, delta));
        }
        theirs_changes.sort_by_key(|(pos, _)| *pos);
        
        // Same for ours
        let mut ours_changes: Vec<(u32, i32)> = Vec::new();
        for hunk in &self.ours_hunks {
            if hunk.status != HunkStatus::Pending {
                continue;
            }
            let base_lines = (hunk.base_rows.end - hunk.base_rows.start) as i32;
            let ours_lines = (hunk.source_rows.end - hunk.source_rows.start) as i32;
            let delta = ours_lines - base_lines;
            ours_changes.push((hunk.base_rows.start, delta));
        }
        ours_changes.sort_by_key(|(pos, _)| *pos);
        
        // Helper function to compute the theirs row corresponding to a base row
        fn base_to_theirs(base_row: u32, theirs_changes: &[(u32, i32)]) -> u32 {
            let mut offset: i32 = 0;
            for &(change_pos, delta) in theirs_changes {
                if change_pos <= base_row {
                    offset += delta;
                } else {
                    break;
                }
            }
            ((base_row as i32) + offset).max(0) as u32
        }
        
        fn base_to_ours(base_row: u32, ours_changes: &[(u32, i32)]) -> u32 {
            let mut offset: i32 = 0;
            for &(change_pos, delta) in ours_changes {
                if change_pos <= base_row {
                    offset += delta;
                } else {
                    break;
                }
            }
            ((base_row as i32) + offset).max(0) as u32
        }
        
        let mut theirs_padding: Vec<(u32, u32)> = Vec::new();
        let mut base_padding: Vec<(u32, u32, bool)> = Vec::new(); // (row, count, is_theirs_color)
        let mut ours_padding: Vec<(u32, u32)> = Vec::new();

        for region in &merged_regions {
            let base_lines = region.base_end.saturating_sub(region.base_start);
            
            // Calculate theirs lines for this region
            // If theirs has changes, use the changed line count; otherwise use base lines
            let theirs_lines: u32 = if region.theirs_ranges.is_empty() {
                base_lines // No change in theirs, same as base
            } else {
                // Sum all theirs ranges (though typically just one per region)
                region.theirs_ranges.iter()
                    .map(|r| r.end.saturating_sub(r.start))
                    .sum()
            };
            
            // Calculate ours lines for this region
            let ours_lines: u32 = if region.ours_ranges.is_empty() {
                base_lines // No change in ours, same as base
            } else {
                region.ours_ranges.iter()
                    .map(|r| r.end.saturating_sub(r.start))
                    .sum()
            };
            
            let max_lines = theirs_lines.max(ours_lines).max(base_lines);
            
            if max_lines == 0 {
                continue;
            }

            // Calculate padding insertion positions using proper coordinate mapping
            // Theirs padding
            if theirs_lines < max_lines {
                let padding_count = max_lines - theirs_lines;
                let insert_row = if let Some(r) = region.theirs_ranges.last() {
                    r.end
                } else {
                    // No theirs change in this region - map base position to theirs coordinates
                    base_to_theirs(region.base_end, &theirs_changes)
                };
                theirs_padding.push((insert_row, padding_count));
            }

            // Base padding
            if base_lines < max_lines {
                let padding_count = max_lines - base_lines;
                let insert_row = region.base_end;
                let is_theirs_dominant = theirs_lines >= ours_lines;
                base_padding.push((insert_row, padding_count, is_theirs_dominant));
            }

            // Ours padding
            if ours_lines < max_lines {
                let padding_count = max_lines - ours_lines;
                let insert_row = if let Some(r) = region.ours_ranges.last() {
                    r.end
                } else {
                    // No ours change in this region - map base position to ours coordinates
                    base_to_ours(region.base_end, &ours_changes)
                };
                ours_padding.push((insert_row, padding_count));
            }
        }

        // Apply highlighting to theirs editor
        self.theirs_editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let max_row = snapshot.max_point().row;
            
            for hunk in &self.theirs_hunks {
                if hunk.status != HunkStatus::Pending {
                    continue;
                }
                match hunk.kind {
                    DiffChangeKind::Added | DiffChangeKind::Modified => {
                        // Highlight added/modified lines in theirs
                        if hunk.source_rows.start < hunk.source_rows.end && hunk.source_rows.start <= max_row {
                            let end_row = hunk.source_rows.end.min(max_row + 1);
                            let start = snapshot.anchor_before(Point::new(hunk.source_rows.start, 0));
                            let end = snapshot.anchor_after(Point::new(end_row, 0));
                            editor.highlight_rows::<TheirsHighlight>(
                                start..end,
                                theirs_addition_color,
                                highlight_options,
                                cx,
                            );
                        }
                    }
                    DiffChangeKind::Deleted => {
                        // For deleted lines, we show a marker but no content to highlight
                    }
                }
            }
        });

        // Apply highlighting to ours editor
        self.ours_editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let max_row = snapshot.max_point().row;
            
            for hunk in &self.ours_hunks {
                if hunk.status != HunkStatus::Pending {
                    continue;
                }
                match hunk.kind {
                    DiffChangeKind::Added | DiffChangeKind::Modified => {
                        if hunk.source_rows.start < hunk.source_rows.end && hunk.source_rows.start <= max_row {
                            let end_row = hunk.source_rows.end.min(max_row + 1);
                            let start = snapshot.anchor_before(Point::new(hunk.source_rows.start, 0));
                            let end = snapshot.anchor_after(Point::new(end_row, 0));
                            editor.highlight_rows::<OursHighlight>(
                                start..end,
                                ours_addition_color,
                                highlight_options,
                                cx,
                            );
                        }
                    }
                    DiffChangeKind::Deleted => {}
                }
            }
        });

        // Apply highlighting to base editor for conflict regions
        // Base shows the original content that will be replaced
        self.base_editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let max_row = snapshot.max_point().row;
            
            // Highlight regions in base that correspond to theirs changes
            for hunk in &self.theirs_hunks {
                if hunk.status != HunkStatus::Pending {
                    continue;
                }
                // For base, highlight the base_rows range (what's being replaced/deleted)
                if hunk.base_rows.start < hunk.base_rows.end && hunk.base_rows.start <= max_row {
                    let end_row = hunk.base_rows.end.min(max_row + 1);
                    let start = snapshot.anchor_before(Point::new(hunk.base_rows.start, 0));
                    let end = snapshot.anchor_before(Point::new(end_row, 0));
                    editor.highlight_rows::<BaseHighlight>(
                        start..end,
                        theirs_addition_color,
                        highlight_options,
                        cx,
                    );
                }
            }
            
            // Highlight regions in base that correspond to ours changes
            for hunk in &self.ours_hunks {
                if hunk.status != HunkStatus::Pending {
                    continue;
                }
                if hunk.base_rows.start < hunk.base_rows.end && hunk.base_rows.start <= max_row {
                    let end_row = hunk.base_rows.end.min(max_row + 1);
                    let start = snapshot.anchor_before(Point::new(hunk.base_rows.start, 0));
                    let end = snapshot.anchor_before(Point::new(end_row, 0));
                    editor.highlight_rows::<BaseHighlight>(
                        start..end,
                        ours_addition_color,
                        highlight_options,
                        cx,
                    );
                }
            }
        });

        // Apply padding blocks for alignment
        // Theirs padding: padding in Theirs editor (matching Base's extra lines)
        for (row, count) in theirs_padding {
            self.insert_padding_block_with_color(&self.theirs_editor.clone(), row, count, PaddingTarget::Theirs, None, cx);
        }
        // Ours padding: padding in Ours editor (matching Base's extra lines)
        for (row, count) in ours_padding {
            self.insert_padding_block_with_color(&self.ours_editor.clone(), row, count, PaddingTarget::Ours, None, cx);
        }
        // Base padding: highlighted padding in Base showing where Theirs/Ours content goes
        for (row, count, is_theirs_color) in base_padding {
            let color = if is_theirs_color { theirs_addition_color } else { ours_addition_color };
            self.insert_padding_block_with_color(&self.base_editor.clone(), row, count, PaddingTarget::Base, Some(color), cx);
        }
    }

    /// Compute diff hunks between base and target text
    fn compute_diff_hunks(&self, base_text: &str, target_text: &str, side: MergeSide) -> Vec<MergeHunk> {
        let mut hunks = Vec::new();
        
        let diff = TextDiff::from_lines(base_text, target_text);
        
        // Use ops() instead of grouped_ops() to get all operations.
        // Use old_index/new_index from DiffOp directly for accurate row positions.
        for op in diff.ops() {
            match *op {
                similar::DiffOp::Equal { .. } => {
                    // Equal regions don't need highlighting
                }
                similar::DiffOp::Delete { old_index, old_len, new_index } => {
                    // Lines exist in base but not in target
                    let base_start = old_index as u32;
                    let base_end = (old_index + old_len) as u32;
                    let target_row = new_index as u32;
                    
                    hunks.push(MergeHunk {
                        side,
                        kind: DiffChangeKind::Deleted,
                        source_rows: target_row..target_row, // No lines in target
                        base_rows: base_start..base_end,
                        text: String::new(),
                        status: HunkStatus::Pending,
                    });
                }
                similar::DiffOp::Insert { old_index, new_index, new_len } => {
                    // Lines exist in target but not in base
                    let target_start = new_index as u32;
                    let target_end = (new_index + new_len) as u32;
                    let base_row = old_index as u32;
                    
                    // Get the inserted text
                    let text: String = target_text.lines()
                        .skip(target_start as usize)
                        .take(new_len)
                        .collect::<Vec<_>>()
                        .join("\n");
                    
                    hunks.push(MergeHunk {
                        side,
                        kind: DiffChangeKind::Added,
                        source_rows: target_start..target_end,
                        base_rows: base_row..base_row, // No lines in base
                        text,
                        status: HunkStatus::Pending,
                    });
                }
                similar::DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                    // Lines modified
                    let base_start = old_index as u32;
                    let base_end = (old_index + old_len) as u32;
                    let target_start = new_index as u32;
                    let target_end = (new_index + new_len) as u32;
                    
                    let text: String = target_text.lines()
                        .skip(target_start as usize)
                        .take(new_len)
                        .collect::<Vec<_>>()
                        .join("\n");
                    
                    hunks.push(MergeHunk {
                        side,
                        kind: DiffChangeKind::Modified,
                        source_rows: target_start..target_end,
                        base_rows: base_start..base_end,
                        text,
                        status: HunkStatus::Pending,
                    });
                }
            }
        }
        
        hunks
    }

    /// Insert a padding block for alignment with optional highlight color
    fn insert_padding_block_with_color(
        &mut self,
        editor: &Entity<Editor>,
        at_row: u32,
        line_count: u32,
        target: PaddingTarget,
        highlight_color: Option<gpui::Hsla>,
        cx: &mut Context<Self>,
    ) {
        if line_count == 0 {
            return;
        }

        let block_ids = editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let max_point = snapshot.max_point();
            let row = at_row.min(max_point.row);
            let anchor = snapshot.anchor_before(Point::new(row, 0));

            editor.insert_blocks(
                [BlockProperties {
                    placement: BlockPlacement::Above(anchor),
                    height: Some(line_count),
                    style: BlockStyle::Fixed,
                    render: Arc::new(move |bx: &mut BlockContext| {
                        let theme = bx.theme();
                        // Use highlight color if provided, otherwise use transparent background
                        let bg_color = highlight_color.unwrap_or(theme.colors().editor_background.opacity(0.0));
                        div()
                            .id(bx.block_id)
                            .w_full()
                            .h(bx.line_height * line_count as f32)
                            .bg(bg_color)
                            .into_any_element()
                    }),
                    priority: 0,
                }],
                None,
                cx,
            )
        });

        // Track block IDs
        match target {
            PaddingTarget::Theirs => self.theirs_alignment_blocks.extend(block_ids),
            PaddingTarget::Base => self.base_alignment_blocks.extend(block_ids),
            PaddingTarget::Ours => self.ours_alignment_blocks.extend(block_ids),
        }
    }

    /// Clear all alignment blocks and highlights
    fn clear_alignment_blocks(&mut self, cx: &mut Context<Self>) {
        // Clear theirs blocks and highlights
        let theirs_blocks = std::mem::take(&mut self.theirs_alignment_blocks);
        self.theirs_editor.update(cx, |editor, cx| {
            editor.clear_row_highlights::<TheirsHighlight>();
            if !theirs_blocks.is_empty() {
                editor.remove_blocks(theirs_blocks.into_iter().collect(), None, cx);
            }
        });

        // Clear base blocks and highlights
        let base_blocks = std::mem::take(&mut self.base_alignment_blocks);
        self.base_editor.update(cx, |editor, cx| {
            editor.clear_row_highlights::<BaseHighlight>();
            if !base_blocks.is_empty() {
                editor.remove_blocks(base_blocks.into_iter().collect(), None, cx);
            }
        });

        // Clear ours blocks and highlights
        let ours_blocks = std::mem::take(&mut self.ours_alignment_blocks);
        self.ours_editor.update(cx, |editor, cx| {
            editor.clear_row_highlights::<OursHighlight>();
            if !ours_blocks.is_empty() {
                editor.remove_blocks(ours_blocks.into_iter().collect(), None, cx);
            }
        });

        // Clear hunk tracking
        self.theirs_hunks.clear();
        self.ours_hunks.clear();
    }

    /// Accept a hunk from theirs side into base
    fn accept_theirs_hunk(&mut self, hunk_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let hunk = match self.theirs_hunks.get(hunk_index) {
            Some(h) if h.status == HunkStatus::Pending => h.clone(),
            _ => return,
        };
        
        // Apply the hunk to base buffer
        self.apply_hunk_to_base(&hunk, cx);
        
        // Mark as accepted
        if let Some(h) = self.theirs_hunks.get_mut(hunk_index) {
            h.status = HunkStatus::Accepted;
        }
        
        // Recalculate hunks after edit (row offsets may have changed)
        self.update_alignment_and_highlighting(window, cx);
        cx.notify();
    }

    /// Accept a hunk from ours side into base
    fn accept_ours_hunk(&mut self, hunk_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let hunk = match self.ours_hunks.get(hunk_index) {
            Some(h) if h.status == HunkStatus::Pending => h.clone(),
            _ => return,
        };
        
        // Apply the hunk to base buffer
        self.apply_hunk_to_base(&hunk, cx);
        
        // Mark as accepted
        if let Some(h) = self.ours_hunks.get_mut(hunk_index) {
            h.status = HunkStatus::Accepted;
        }
        
        // Recalculate hunks after edit
        self.update_alignment_and_highlighting(window, cx);
        cx.notify();
    }

    /// Apply a hunk's content to the base buffer
    fn apply_hunk_to_base(&mut self, hunk: &MergeHunk, cx: &mut Context<Self>) {
        self.base_buffer.update(cx, |buffer, cx| {
            let snapshot = buffer.snapshot();
            let max_point = snapshot.max_point();
            
            match hunk.kind {
                DiffChangeKind::Added => {
                    // Insert new lines at the base position
                    let insert_row = hunk.base_rows.start.min(max_point.row);
                    let insert_point = Point::new(insert_row, 0);
                    let insert_offset = snapshot.point_to_offset(insert_point);
                    
                    // Add newline if text doesn't end with one
                    let text_to_insert = if hunk.text.ends_with('\n') {
                        hunk.text.clone()
                    } else {
                        format!("{}\n", hunk.text)
                    };
                    
                    buffer.edit([(insert_offset..insert_offset, text_to_insert)], None, cx);
                }
                DiffChangeKind::Deleted => {
                    // Delete lines from base (this is usually a no-op for accept,
                    // since we're accepting that theirs/ours removed these lines)
                    // For merge, we typically just mark it as accepted
                }
                DiffChangeKind::Modified => {
                    // Replace the base range with the hunk text
                    let start_row = hunk.base_rows.start.min(max_point.row);
                    let end_row = hunk.base_rows.end.min(max_point.row + 1);
                    
                    let start_point = Point::new(start_row, 0);
                    let end_point = if end_row > max_point.row {
                        max_point
                    } else {
                        Point::new(end_row, 0)
                    };
                    
                    let start_offset = snapshot.point_to_offset(start_point);
                    let end_offset = snapshot.point_to_offset(end_point);
                    
                    // Add newline if text doesn't end with one
                    let text_to_insert = if hunk.text.ends_with('\n') || end_row > max_point.row {
                        hunk.text.clone()
                    } else {
                        format!("{}\n", hunk.text)
                    };
                    
                    buffer.edit([(start_offset..end_offset, text_to_insert)], None, cx);
                }
            }
        });
    }

    /// Ignore a hunk from theirs side
    fn ignore_theirs_hunk(&mut self, hunk_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(hunk) = self.theirs_hunks.get_mut(hunk_index) {
            hunk.status = HunkStatus::Ignored;
            self.update_alignment_and_highlighting(window, cx);
            cx.notify();
        }
    }

    /// Ignore a hunk from ours side
    fn ignore_ours_hunk(&mut self, hunk_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(hunk) = self.ours_hunks.get_mut(hunk_index) {
            hunk.status = HunkStatus::Ignored;
            self.update_alignment_and_highlighting(window, cx);
            cx.notify();
        }
    }

    /// Accept all pending hunks from theirs side
    fn accept_all_theirs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for hunk in &mut self.theirs_hunks {
            if hunk.status == HunkStatus::Pending {
                hunk.status = HunkStatus::Accepted;
            }
        }
        self.update_alignment_and_highlighting(window, cx);
        cx.notify();
    }

    /// Accept all pending hunks from ours side
    fn accept_all_ours(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for hunk in &mut self.ours_hunks {
            if hunk.status == HunkStatus::Pending {
                hunk.status = HunkStatus::Accepted;
            }
        }
        self.update_alignment_and_highlighting(window, cx);
        cx.notify();
    }

    /// Check if there are pending theirs hunks
    fn has_pending_theirs(&self) -> bool {
        self.theirs_hunks.iter().any(|h| h.status == HunkStatus::Pending)
    }

    /// Check if there are pending ours hunks
    fn has_pending_ours(&self) -> bool {
        self.ours_hunks.iter().any(|h| h.status == HunkStatus::Pending)
    }

    /// Mark the conflict as resolved
    fn mark_as_resolved(&mut self, _: &MarkAsResolved, _window: &mut Window, cx: &mut Context<Self>) {
        if self.all_hunks_processed() {
            // Emit event to close the editor and mark conflict as resolved
            cx.emit(ItemEvent::CloseItem);
        }
    }

    /// Render the left divider with hunk buttons (between Ours and Base)
    fn render_left_divider(&self, line_height: f32, scroll_y: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let border_color = cx.theme().colors().border;
        let editor_bg = cx.theme().colors().editor_background;
        let ours_color = cx.theme().colors().version_control_conflict_marker_ours.opacity(0.20);
        
        // Get visible hunks for ours side (left panel)
        let visible_hunks = self.get_visible_ours_hunks(line_height, scroll_y);
        
        div()
            .id("left-divider")
            .w(DIVIDER_WIDTH)
            .h_full()
            .relative()
            .overflow_hidden()
            .cursor_col_resize()
            .bg(editor_bg)
            .border_l_1()
            .border_r_1()
            .border_color(border_color)
            .on_drag(DraggedLeftDivider, |_, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DraggedLeftDivider)
            })
            // Highlight regions extending from ours side (left half of divider)
            .children(visible_hunks.iter().filter(|h| h.is_pending).map(|hunk| {
                let top = hunk.top_offset + 24.0; // Skip header
                let height = hunk.height;
                div()
                    .absolute()
                    .top(px(top.max(24.0)))
                    .left_0()
                    .w(DIVIDER_WIDTH / 2.0) // Left half for ours
                    .h(px(height))
                    .bg(ours_color)
            }))
            // Hunk buttons overlay
            .children(visible_hunks.into_iter().filter(|h| h.is_pending).map(|hunk| {
                let idx = hunk.index;
                let button_top = hunk.top_offset + hunk.height / 2.0 - 10.0 + 24.0; // Center vertically, skip header
                
                div()
                    .id(SharedString::from(format!("ours-hunk-{}", idx)))
                    .absolute()
                    .top(px(button_top.max(24.0)))
                    .left_0()
                    .w_full()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_0p5()
                    .child(
                        IconButton::new(("accept-ours", idx), IconName::ArrowRight)
                            .icon_size(ui::IconSize::XSmall)
                            .icon_color(Color::Accent)
                            .tooltip(Tooltip::text("Accept this change"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.accept_ours_hunk(idx, window, cx);
                            }))
                    )
                    .child(
                        IconButton::new(("ignore-ours", idx), IconName::Close)
                            .icon_size(ui::IconSize::XSmall)
                            .icon_color(Color::Muted)
                            .tooltip(Tooltip::text("Ignore this change"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.ignore_ours_hunk(idx, window, cx);
                            }))
                    )
            }))
    }

    /// Render the right divider with hunk buttons (between Base and Theirs)
    fn render_right_divider(&self, line_height: f32, scroll_y: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let border_color = cx.theme().colors().border;
        let editor_bg = cx.theme().colors().editor_background;
        let theirs_color = cx.theme().colors().version_control_conflict_marker_theirs.opacity(0.20);
        
        // Get visible hunks for theirs side (right panel)
        let visible_hunks = self.get_visible_theirs_hunks(line_height, scroll_y);
        
        div()
            .id("right-divider")
            .w(DIVIDER_WIDTH)
            .h_full()
            .relative()
            .overflow_hidden()
            .cursor_col_resize()
            .bg(editor_bg)
            .border_l_1()
            .border_r_1()
            .border_color(border_color)
            .on_drag(DraggedRightDivider, |_, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DraggedRightDivider)
            })
            // Highlight regions extending from theirs side (right half of divider)
            .children(visible_hunks.iter().filter(|h| h.is_pending).map(|hunk| {
                let top = hunk.top_offset + 24.0; // Skip header
                let height = hunk.height;
                div()
                    .absolute()
                    .top(px(top.max(24.0)))
                    .right_0()
                    .w(DIVIDER_WIDTH / 2.0) // Right half for theirs
                    .h(px(height))
                    .bg(theirs_color)
            }))
            // Hunk buttons overlay
            .children(visible_hunks.into_iter().filter(|h| h.is_pending).map(|hunk| {
                let idx = hunk.index;
                let button_top = hunk.top_offset + hunk.height / 2.0 - 10.0 + 24.0; // Center vertically, skip header
                
                div()
                    .id(SharedString::from(format!("theirs-hunk-{}", idx)))
                    .absolute()
                    .top(px(button_top.max(24.0)))
                    .left_0()
                    .w_full()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_0p5()
                    .child(
                        IconButton::new(("ignore-theirs", idx), IconName::Close)
                            .icon_size(ui::IconSize::XSmall)
                            .icon_color(Color::Muted)
                            .tooltip(Tooltip::text("Ignore this change"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.ignore_theirs_hunk(idx, window, cx);
                            }))
                    )
                    .child(
                        IconButton::new(("accept-theirs", idx), IconName::ArrowLeft)
                            .icon_size(ui::IconSize::XSmall)
                            .icon_color(Color::Accent)
                            .tooltip(Tooltip::text("Accept this change"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.accept_theirs_hunk(idx, window, cx);
                            }))
                    )
            }))
    }
}

impl Render for ThreeWayMergeEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Get line height and scroll position before borrowing theme
        // Use a reasonable default line height based on rem size
        let rem_size: f32 = window.rem_size().into();
        let line_height: f32 = rem_size * 1.5; // Approximate line height
        let scroll_y = self.base_editor.update(cx, |editor, cx| {
            editor.snapshot(window, cx).scroll_position().y as f32
        });
        
        let theme = cx.theme();
        let theirs_name = self.conflict.theirs_branch_name.clone();
        let ours_name = self.conflict.ours_branch_name.clone();
        let relative_path = self.path.to_string_lossy().to_string();
        
        // Navigation state
        let (has_prev, has_next) = self.diff_navigation_state(cx);
        let pending_count = self.pending_diff_count();
        let all_processed = self.all_hunks_processed();
        let is_resolve_mode = self.is_resolve_mode;
        let has_pending_theirs = self.has_pending_theirs();
        let has_pending_ours = self.has_pending_ours();
        
        let focus_handle = self.focus_handle.clone();
        
        // Panel ratios
        let theirs_ratio = self.theirs_ratio;
        let ours_ratio = self.ours_ratio;
        let base_ratio = 1.0 - theirs_ratio - ours_ratio;
        
        let border_color = theme.colors().border;
        let title_bar_bg = theme.colors().title_bar_background;
        let editor_bg = theme.colors().editor_background;
        let theirs_header_bg = theme.colors().version_control_conflict_marker_theirs.opacity(0.3);
        let ours_header_bg = theme.colors().version_control_conflict_marker_ours.opacity(0.3);
        let surface_bg = theme.colors().surface_background;

        div()
            .id("three-way-merge-editor")
            .track_focus(&self.focus_handle)
            .key_context("ThreeWayMergeEditor")
            .on_action(cx.listener(Self::go_to_next_diff))
            .on_action(cx.listener(Self::go_to_previous_diff))
            .on_action(cx.listener(Self::toggle_resolve_mode))
            .on_action(cx.listener(Self::mark_as_resolved))
            .size_full()
            .flex()
            .flex_col()
            .bg(editor_bg)
            // Top header bar
            .child(
                div()
                    .h(px(32.))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(title_bar_bg)
                    .border_b_1()
                    .border_color(border_color)
                    // Left: file path
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::new(IconName::GitBranch)
                                    .size(ui::IconSize::Small)
                                    .color(Color::Conflict),
                            )
                            .child(
                                Label::new(relative_path)
                                    .size(LabelSize::Small)
                                    .color(Color::Default),
                            )
                    )
                    // Right: navigation and action buttons
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            // Diff count
                            .child(
                                Label::new(if pending_count == 1 {
                                    "1 conflict".to_string()
                                } else {
                                    format!("{} conflicts", pending_count)
                                })
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            // Navigation buttons
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(
                                        IconButton::new("prev-diff", IconName::ArrowUp)
                                            .icon_size(ui::IconSize::Small)
                                            .tooltip(Tooltip::for_action_title_in(
                                                "Previous Diff",
                                                &GoToPreviousDiff,
                                                &focus_handle,
                                            ))
                                            .disabled(!has_prev)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.navigate_to_diff(false, window, cx);
                                            })),
                                    )
                                    .child(
                                        IconButton::new("next-diff", IconName::ArrowDown)
                                            .icon_size(ui::IconSize::Small)
                                            .tooltip(Tooltip::for_action_title_in(
                                                "Next Diff",
                                                &GoToNextDiff,
                                                &focus_handle,
                                            ))
                                            .disabled(!has_next)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.navigate_to_diff(true, window, cx);
                                            })),
                                    )
                            )
                            // Accept All buttons (Ours on left, Theirs on right)
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(
                                        IconButton::new("accept-all-ours", IconName::ArrowRight)
                                            .icon_size(ui::IconSize::Small)
                                            .icon_color(Color::Accent)
                                            .tooltip(Tooltip::text("Accept All from Ours (Left)"))
                                            .disabled(!has_pending_ours)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.accept_all_ours(window, cx);
                                            })),
                                    )
                                    .child(
                                        IconButton::new("accept-all-theirs", IconName::ArrowLeft)
                                            .icon_size(ui::IconSize::Small)
                                            .icon_color(Color::Accent)
                                            .tooltip(Tooltip::text("Accept All from Theirs (Right)"))
                                            .disabled(!has_pending_theirs)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.accept_all_theirs(window, cx);
                                            })),
                                    )
                            )
                            // Read/Resolve View toggle
                            .child(
                                IconButton::new(
                                    "toggle-mode",
                                    if is_resolve_mode { IconName::Pencil } else { IconName::Eye },
                                )
                                    .icon_size(ui::IconSize::Small)
                                    .tooltip(Tooltip::text(if is_resolve_mode {
                                        "Switch to Read View"
                                    } else {
                                        "Switch to Resolve View"
                                    }))
                                    .toggle_state(is_resolve_mode)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_resolve_mode(&ToggleResolveMode, window, cx);
                                    })),
                            )
                            // Mark as Resolved
                            .child(
                                IconButton::new("mark-resolved", IconName::Check)
                                    .icon_size(ui::IconSize::Small)
                                    .tooltip(Tooltip::text("Mark As Resolved"))
                                    .disabled(!all_processed)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.mark_as_resolved(&MarkAsResolved, window, cx);
                                    })),
                            )
                    )
            )
            // Three-panel editors container
            .child({
                div()
                    .id("editors-container")
                    .flex_1()
                    .flex()
                    .flex_row()
                    // Handle left divider drag
                    .on_drag_move(cx.listener(move |this, e: &DragMoveEvent<DraggedLeftDivider>, _window, cx| {
                        let container_width: f32 = e.bounds.size.width.into();
                        if container_width > 0.0 {
                            let position_x: f32 = e.event.position.x.into();
                            let origin_x: f32 = e.bounds.origin.x.into();
                            let relative_x = position_x - origin_x;
                            // Calculate theirs ratio, keeping base and ours proportionally
                            let new_theirs_ratio = (relative_x / container_width).clamp(0.15, 0.5);
                            // Adjust ours ratio proportionally to fill remaining space
                            let remaining = 1.0 - new_theirs_ratio;
                            let old_base_ours = 1.0 - this.theirs_ratio;
                            if old_base_ours > 0.0 {
                                let ours_proportion = this.ours_ratio / old_base_ours;
                                this.ours_ratio = (remaining * ours_proportion).clamp(0.15, 0.5);
                            }
                            this.theirs_ratio = new_theirs_ratio;
                            cx.notify();
                        }
                    }))
                    // Handle right divider drag
                    .on_drag_move(cx.listener(move |this, e: &DragMoveEvent<DraggedRightDivider>, _window, cx| {
                        let container_width: f32 = e.bounds.size.width.into();
                        if container_width > 0.0 {
                            let position_x: f32 = e.event.position.x.into();
                            let origin_x: f32 = e.bounds.origin.x.into();
                            let relative_x = position_x - origin_x;
                            // Calculate ours ratio (from right edge)
                            let new_ours_ratio = (1.0 - relative_x / container_width).clamp(0.15, 0.5);
                            // Adjust theirs ratio proportionally
                            let remaining = 1.0 - new_ours_ratio;
                            let old_theirs_base = 1.0 - this.ours_ratio;
                            if old_theirs_base > 0.0 {
                                let theirs_proportion = this.theirs_ratio / old_theirs_base;
                                this.theirs_ratio = (remaining * theirs_proportion).clamp(0.15, 0.5);
                            }
                            this.ours_ratio = new_ours_ratio;
                            cx.notify();
                        }
                    }))
                    // Left panel: Ours (current branch)
                    .child(
                        div()
                            .flex_grow()
                            .flex_shrink()
                            .flex_basis(relative(ours_ratio))
                            .min_w(px(100.))
                            .flex()
                            .flex_col()
                            // Header
                            .child(
                                div()
                                    .h(px(24.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .bg(ours_header_bg)
                                    .border_b_1()
                                    .border_color(border_color)
                                    .child(
                                        Label::new(format!("{} (Ours)", ours_name))
                                            .size(LabelSize::XSmall)
                                            .color(Color::Default),
                                    )
                            )
                            // Editor
                            .child(
                                div()
                                    .flex_1()
                                    .child(self.ours_editor.clone())
                            )
                    )
                    // Left divider with hunk buttons
                    .child(self.render_left_divider(line_height, scroll_y, cx))
                    // Center panel: Base
                    .child(
                        div()
                            .flex_grow()
                            .flex_shrink()
                            .flex_basis(relative(base_ratio))
                            .min_w(px(100.))
                            .flex()
                            .flex_col()
                            // Header
                            .child(
                                div()
                                    .h(px(24.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .bg(surface_bg)
                                    .border_b_1()
                                    .border_color(border_color)
                                    .child(
                                        Label::new(if is_resolve_mode {
                                            "Base (Editable)"
                                        } else {
                                            "Base (Read-only)"
                                        })
                                            .size(LabelSize::XSmall)
                                            .color(if is_resolve_mode {
                                                Color::Accent
                                            } else {
                                                Color::Muted
                                            }),
                                    )
                            )
                            // Editor
                            .child(
                                div()
                                    .flex_1()
                                    .child(self.base_editor.clone())
                            )
                    )
                    // Right divider with hunk buttons
                    .child(self.render_right_divider(line_height, scroll_y, cx))
                    // Right panel: Theirs (incoming branch)
                    .child(
                        div()
                            .flex_grow()
                            .flex_shrink()
                            .flex_basis(relative(theirs_ratio))
                            .min_w(px(100.))
                            .flex()
                            .flex_col()
                            // Header
                            .child(
                                div()
                                    .h(px(24.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .bg(theirs_header_bg)
                                    .border_b_1()
                                    .border_color(border_color)
                                    .child(
                                        Label::new(format!("{} (Theirs)", theirs_name))
                                            .size(LabelSize::XSmall)
                                            .color(Color::Default),
                                    )
                            )
                            // Editor
                            .child(
                                div()
                                    .flex_1()
                                    .child(self.theirs_editor.clone())
                            )
                    )
            })
    }
}

impl Focusable for ThreeWayMergeEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<ItemEvent> for ThreeWayMergeEditor {}

impl Item for ThreeWayMergeEditor {
    type Event = ItemEvent;

    fn tab_content(&self, params: TabContentParams, _window: &Window, _cx: &App) -> AnyElement {
        let label = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Merge".to_string());

        Label::new(format!("⚡ {}", label))
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::GitBranch).color(Color::Conflict))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        let label = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Merge".to_string());
        format!("⚡ {}", label).into()
    }

    fn to_item_events(event: &Self::Event, mut f: impl FnMut(ItemEvent)) {
        f(*event)
    }

    fn deactivated(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn navigate(&mut self, _: Box<dyn Any>, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        false
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(format!("3-Way Merge: {}", self.path.display()).into())
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.base_editor.read(cx).is_dirty(cx)
    }

    fn has_conflict(&self, _cx: &App) -> bool {
        !self.all_hunks_processed()
    }

    fn can_save(&self, _cx: &App) -> bool {
        true
    }

    fn save(
        &mut self,
        _options: workspace::item::SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.base_editor.update(cx, |editor, cx| {
            editor.save(workspace::item::SaveOptions::default(), project, window, cx)
        })
    }

    fn save_as(
        &mut self,
        project: Entity<Project>,
        path: ProjectPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.base_editor.update(cx, |editor, cx| {
            editor.save_as(project, path, window, cx)
        })
    }

    fn reload(
        &mut self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.base_editor.update(cx, |editor, cx| {
            editor.reload(project, window, cx)
        })
    }

    fn clone_on_split(
        &self,
        _workspace_id: Option<workspace::WorkspaceId>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>>
    where
        Self: Sized,
    {
        Task::ready(None)
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.base_editor.update(cx, |editor, _cx| {
            editor.set_nav_history(Some(nav_history));
        });
    }
}
