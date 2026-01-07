//! IntelliJ-style Side-by-Side Diff View
//!
//! This module provides a true side-by-side diff view with:
//! - Left editor: base/old version (read-only)
//! - Right editor: worktree/new version (editable)
//! - Line alignment: matching lines appear at the same vertical position
//! - Synchronized scrolling: both editors scroll together
//! - Visual diff markers: additions, deletions, and modifications are highlighted

use anyhow::Result;
use buffer_diff::{BufferDiff, BufferDiffEvent, DiffHunkStatusKind};
use editor::{
    Editor, EditorEvent, ExcerptRange, RowHighlightOptions, ToPoint,
    display_map::{BlockPlacement, BlockProperties, BlockStyle, CustomBlockId},
    scroll::Autoscroll,
};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render, Styled, Subscription, Task,
    Window, actions, div,
};
use language::{Buffer, Capability, Point};
use multi_buffer::MultiBuffer;
use project::Project;
use std::{
    any::Any,
    cell::Cell,
    path::PathBuf,
    sync::Arc,
};
use ui::{
    ActiveTheme, Color, Icon, IconButton, IconName, Label, LabelCommon as _, SharedString,
    Tooltip, prelude::*,
};
use workspace::{
    Item, ItemNavHistory, Workspace,
    item::{ItemEvent, TabContentParams},
};

// Actions for hunk navigation in side-by-side diff view
actions!(
    side_by_side_diff,
    [
        GoToNextHunk,
        GoToPreviousHunk,
    ]
);

/// Register keybindings for side-by-side diff view
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("alt-]", GoToNextHunk, Some("SideBySideDiffView")),
        KeyBinding::new("alt-[", GoToPreviousHunk, Some("SideBySideDiffView")),
    ]);
}

/// Marker type for deletion highlighting in left editor (row-level)
struct DeletionHighlight;

/// Marker type for addition highlighting in right editor (row-level)
struct AdditionHighlight;

/// Marker type for modification highlighting (row-level)
struct ModificationHighlight;

/// Marker type for word-level deletion highlighting in left editor
struct WordDeletionHighlight;

/// Marker type for word-level addition highlighting in right editor
struct WordAdditionHighlight;

/// IntelliJ-style side-by-side diff view
#[allow(dead_code)]
pub struct SideBySideDiffView {
    /// Left editor showing base/old version (read-only)
    left_editor: Entity<Editor>,
    /// Right editor showing worktree/new version (editable)
    right_editor: Entity<Editor>,
    /// The buffer diff calculation
    diff: Entity<BufferDiff>,
    /// The old buffer (base version)
    old_buffer: Entity<Buffer>,
    /// The new buffer (worktree version)
    new_buffer: Entity<Buffer>,
    /// Path being compared
    path: PathBuf,
    /// Label for the base/left side (e.g., "HEAD", branch name, or commit hash)
    base_label: String,
    /// Focus handle for the view
    focus_handle: FocusHandle,
    /// Prevent recursive scroll sync
    is_syncing_scroll: Cell<bool>,
    /// Alignment blocks inserted in left editor
    left_alignment_blocks: Vec<CustomBlockId>,
    /// Alignment blocks inserted in right editor
    right_alignment_blocks: Vec<CustomBlockId>,
    /// Subscriptions
    _subscriptions: Vec<Subscription>,
}


impl SideBySideDiffView {
    /// Create a new side-by-side diff view for a file
    pub fn new(
        old_buffer: Entity<Buffer>,
        new_buffer: Entity<Buffer>,
        diff: Entity<BufferDiff>,
        path: PathBuf,
        base_label: String,
        project: Option<Entity<Project>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // Create left editor (read-only, shows base/old version)
        let left_multibuffer = cx.new(|cx| {
            let mut mb = MultiBuffer::new(Capability::ReadOnly);
            mb.push_excerpts(
                old_buffer.clone(),
                [ExcerptRange::new(text::Anchor::MIN..text::Anchor::MAX)],
                cx,
            );
            mb
        });

        let left_editor = cx.new(|cx| {
            let mut editor = Editor::for_multibuffer(
                left_multibuffer.clone(),
                project.clone(),
                window,
                cx,
            );
            editor.set_read_only(true);
            editor.set_show_gutter(true, cx);
            // Show context menu instead of toggling inline diff when clicking hunk
            editor.set_show_hunk_context_menu_on_click(true);
            editor
        });

        // Create right editor (editable, shows worktree/new version)
        // Note: We don't use add_diff() here because we want to handle highlighting ourselves
        // The built-in diff shows deletions in the new buffer, but we want deletions in the left side
        let right_multibuffer = cx.new(|cx| {
            let mut mb = MultiBuffer::new(Capability::ReadWrite);
            mb.push_excerpts(
                new_buffer.clone(),
                [ExcerptRange::new(text::Anchor::MIN..text::Anchor::MAX)],
                cx,
            );
            mb
        });

        let right_editor = cx.new(|cx| {
            let mut editor = Editor::for_multibuffer(
                right_multibuffer.clone(),
                project.clone(),
                window,
                cx,
            );
            editor.set_show_gutter(true, cx);
            // Show context menu instead of toggling inline diff when clicking hunk
            editor.set_show_hunk_context_menu_on_click(true);
            editor
        });

        // Link editors for synchronized hunk navigation
        left_editor.update(cx, |editor, _cx| editor.set_linked_editor(Some(right_editor.clone())));
        right_editor.update(cx, |editor, _cx| editor.set_linked_editor(Some(left_editor.clone())));

        // Subscribe to scroll events for sync
        let mut subscriptions = Vec::new();

        // Left editor scroll -> sync to right
        let right_editor_for_sync = right_editor.clone();
        subscriptions.push(cx.subscribe_in(
            &left_editor,
            window,
            move |this, _, event: &EditorEvent, window, cx| {
                // Match all local scroll events (both autoscroll and manual scroll)
                if let EditorEvent::ScrollPositionChanged { local: true, .. } = event {
                    println!("[SideBySideDiffView] Left editor scrolled: {:?}", event);
                    if !this.is_syncing_scroll.get() {
                        this.is_syncing_scroll.set(true);
                        let scroll_position = this.left_editor.update(cx, |editor, cx| {
                            editor.scroll_position(cx)
                        });
                        right_editor_for_sync.update(cx, |editor, cx| {
                            editor.set_scroll_position(scroll_position, window, cx);
                        });
                        this.is_syncing_scroll.set(false);
                    }
                }
            },
        ));

        // Right editor scroll -> sync to left
        let left_editor_for_sync = left_editor.clone();
        subscriptions.push(cx.subscribe_in(
            &right_editor,
            window,
            move |this, _, event: &EditorEvent, window, cx| {
                // Match all local scroll events (both autoscroll and manual scroll)
                if let EditorEvent::ScrollPositionChanged { local: true, .. } = event {
                    println!("[SideBySideDiffView] Right editor scrolled: {:?}", event);
                    if !this.is_syncing_scroll.get() {
                        this.is_syncing_scroll.set(true);
                        let scroll_position = this.right_editor.update(cx, |editor, cx| {
                            editor.scroll_position(cx)
                        });
                        left_editor_for_sync.update(cx, |editor, cx| {
                            editor.set_scroll_position(scroll_position, window, cx);
                        });
                        this.is_syncing_scroll.set(false);
                    }
                }
            },
        ));

        // Subscribe to diff changes to update highlighting when buffer content changes
        subscriptions.push(cx.subscribe_in(
            &diff,
            window,
            move |this, _, event: &BufferDiffEvent, window, cx| {
                if let BufferDiffEvent::DiffChanged { .. } = event {
                    // Recalculate alignment and highlighting when diff changes
                    this.update_alignment_and_highlighting(window, cx);
                }
            },
        ));

        let mut view = Self {
            left_editor,
            right_editor,
            diff,
            old_buffer,
            new_buffer,
            path,
            base_label,
            focus_handle,
            is_syncing_scroll: Cell::new(false),
            left_alignment_blocks: Vec::new(),
            right_alignment_blocks: Vec::new(),
            _subscriptions: subscriptions,
        };

        // Calculate and apply line alignment and highlighting
        view.update_alignment_and_highlighting(window, cx);

        view
    }

    /// Open a side-by-side diff view for a file in the workspace
    pub fn open(
        old_buffer: Entity<Buffer>,
        new_buffer: Entity<Buffer>,
        diff: Entity<BufferDiff>,
        path: PathBuf,
        base_label: String,
        project: Entity<Project>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let view = cx.new(|cx| {
            Self::new(
                old_buffer,
                new_buffer,
                diff,
                path,
                base_label,
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

    /// Calculate and apply alignment blocks and row highlighting
    /// 
    /// This creates a true side-by-side diff where:
    /// - Added lines: left padding + right highlight (green)
    /// - Deleted lines: left highlight (red) + right padding
    /// - Modified lines: left highlight (yellow) + right highlight (yellow)
    /// - Lines are aligned with padding blocks
    fn update_alignment_and_highlighting(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // Clear existing alignment blocks and highlights
        self.clear_alignment_blocks(cx);

        // Get the buffer snapshots
        let _old_buffer_snapshot = self.old_buffer.read(cx).snapshot();
        let new_buffer_snapshot = self.new_buffer.read(cx).snapshot();
        
        // Get the diff snapshot and iterate over hunks
        let diff_snapshot = self.diff.read(cx).snapshot(cx);
        let hunks: Vec<_> = diff_snapshot.hunks(&new_buffer_snapshot).collect();
        
        // Get the base text for calculating line positions in old buffer
        let base_text = diff_snapshot.base_text();
        let base_text_len = base_text.len();
        
        // Helper to calculate line count from byte range in base text
        // A line is defined by content between newlines (or start/end of text)
        let count_lines_in_range = |start_byte: usize, end_byte: usize| -> u32 {
            if start_byte >= end_byte || start_byte >= base_text_len {
                return 0;
            }
            let end_byte = end_byte.min(base_text_len);
            let text: String = base_text.text_for_range(
                base_text.anchor_before(start_byte)..base_text.anchor_after(end_byte)
            ).collect();
            if text.is_empty() {
                return 0;
            }
            // Count the number of lines:
            // - Each \n terminates a line
            // - If text doesn't end with \n, there's one more line
            let newline_count = text.matches('\n').count() as u32;
            if text.ends_with('\n') {
                newline_count
            } else {
                newline_count + 1
            }
        };
        
        // Helper to get line number for a given byte offset
        // Returns the 0-indexed line number at the given byte position
        let byte_to_line = |byte_offset: usize| -> u32 {
            if byte_offset == 0 {
                return 0;
            }
            let byte_offset = byte_offset.min(base_text_len);
            let text: String = base_text.text_for_range(
                base_text.anchor_before(0)..base_text.anchor_before(byte_offset)
            ).collect();
            // The line number is the count of newlines before this position
            text.matches('\n').count() as u32
        };
        
        // Theme colors for highlighting
        let deletion_color = cx.theme().colors().version_control_deleted.opacity(0.25);
        let addition_color = cx.theme().colors().version_control_added.opacity(0.25);
        let modification_color = cx.theme().colors().version_control_modified.opacity(0.25);
        
        let highlight_options = RowHighlightOptions {
            include_gutter: true,
            ..Default::default()
        };
        
        // Highlight kind for tracking which marker type to use
        #[derive(Clone, Copy)]
        enum HighlightKind {
            Deletion,
            Addition,
            Modification,
        }
        
        // Collect all padding and highlighting operations
        // Blocks use anchors which are based on buffer positions, not display positions
        // So we don't need to track cumulative padding
        let mut left_padding_ops: Vec<(u32, u32)> = Vec::new();
        let mut right_padding_ops: Vec<(u32, u32)> = Vec::new();
        let mut left_highlights: Vec<(std::ops::Range<u32>, gpui::Hsla, HighlightKind)> = Vec::new();
        let mut right_highlights: Vec<(std::ops::Range<u32>, gpui::Hsla, HighlightKind)> = Vec::new();
        
        // Word-level diff highlights (for Modified hunks)
        // These are stored as anchors directly from the hunk
        let mut left_word_diffs: Vec<std::ops::Range<usize>> = Vec::new();  // byte ranges in base text
        let mut right_word_diffs: Vec<text::Anchor> = Vec::new();  // anchors in new buffer (start, end pairs)
        
        for hunk in hunks {
            let status = hunk.status();
            
            // Positions in new buffer (from hunk.range)
            let new_start = hunk.range.start.row;
            let new_end = hunk.range.end.row;
            let new_count = new_end.saturating_sub(new_start);
            
            // Positions in old/base buffer (calculated from byte offsets)
            let old_start = byte_to_line(hunk.diff_base_byte_range.start);
            let old_count = count_lines_in_range(hunk.diff_base_byte_range.start, hunk.diff_base_byte_range.end);
            
            // Debug logging - using eprintln for immediate visibility
            eprintln!(
                "[SideBySideDiff] Hunk: kind={:?}, old_start={}, old_count={}, new_start={}, new_end={}, new_count={}, base_byte_range={}..{}",
                status.kind,
                old_start,
                old_count,
                new_start,
                new_end,
                new_count,
                hunk.diff_base_byte_range.start,
                hunk.diff_base_byte_range.end
            );
            
            match status.kind {
                DiffHunkStatusKind::Added => {
                    // New lines only exist in new buffer
                    // Insert padding in left editor at the position where the addition occurs
                    // The padding should appear AFTER the line at old_start (if any)
                    if new_count > 0 {
                        // Padding goes AFTER old_start in left editor
                        // Use old_start as the anchor point - block will be placed above this row
                        left_padding_ops.push((old_start, new_count));
                        
                        // Highlight added lines in right editor
                        right_highlights.push((new_start..new_end, addition_color, HighlightKind::Addition));
                    }
                }
                DiffHunkStatusKind::Deleted => {
                    // Lines only exist in old buffer
                    if old_count > 0 {
                        // Highlight deleted lines in left editor
                        left_highlights.push((old_start..old_start + old_count, deletion_color, HighlightKind::Deletion));
                        
                        // Insert padding in right editor at the position where deletion occurred
                        // The padding appears where the deleted lines would be
                        right_padding_ops.push((new_start, old_count));
                    }
                }
                DiffHunkStatusKind::Modified => {
                    // Both sides have content - highlight both with modification color
                    eprintln!("[SideBySideDiff] Modified hunk: old_count={}, new_count={}, word_diffs: left={}, right={}", 
                        old_count, new_count, hunk.base_word_diffs.len(), hunk.buffer_word_diffs.len());
                    if old_count > 0 {
                        eprintln!("[SideBySideDiff]   Adding LEFT highlight: rows {}..{}", old_start, old_start + old_count);
                        left_highlights.push((old_start..old_start + old_count, modification_color, HighlightKind::Modification));
                    }
                    if new_count > 0 {
                        eprintln!("[SideBySideDiff]   Adding RIGHT highlight: rows {}..{}", new_start, new_end);
                        right_highlights.push((new_start..new_end, modification_color, HighlightKind::Modification));
                    }
                    
                    // Collect word-level diffs for inline highlighting
                    // base_word_diffs are byte offsets relative to the start of the hunk in base text
                    // We need to convert them to absolute byte offsets
                    let hunk_base_start = hunk.diff_base_byte_range.start;
                    for word_range in &hunk.base_word_diffs {
                        let absolute_start = hunk_base_start + word_range.start;
                        let absolute_end = hunk_base_start + word_range.end;
                        left_word_diffs.push(absolute_start..absolute_end);
                    }
                    
                    // buffer_word_diffs are already anchors in the new buffer
                    for word_range in &hunk.buffer_word_diffs {
                        right_word_diffs.push(word_range.start);
                        right_word_diffs.push(word_range.end);
                    }
                    
                    // Add padding to balance line counts
                    if new_count > old_count {
                        // More lines in new - add padding to left after old content
                        let padding_count = new_count - old_count;
                        left_padding_ops.push((old_start + old_count, padding_count));
                    } else if old_count > new_count {
                        // More lines in old - add padding to right after new content
                        let padding_count = old_count - new_count;
                        right_padding_ops.push((new_end, padding_count));
                    }
                }
            }
        }
        
        // Helper macro to apply highlights with correct marker type
        macro_rules! apply_highlight {
            ($editor:expr, $snapshot:expr, $row_range:expr, $color:expr, $kind:expr, $options:expr, $cx:expr) => {
                let start = $snapshot.anchor_before(Point::new($row_range.start, 0));
                let end = $snapshot.anchor_after(Point::new($row_range.end, 0));
                match $kind {
                    HighlightKind::Deletion => {
                        $editor.highlight_rows::<DeletionHighlight>(start..end, $color, $options, $cx);
                    }
                    HighlightKind::Addition => {
                        $editor.highlight_rows::<AdditionHighlight>(start..end, $color, $options, $cx);
                    }
                    HighlightKind::Modification => {
                        $editor.highlight_rows::<ModificationHighlight>(start..end, $color, $options, $cx);
                    }
                }
            };
        }
        
        // Log collected highlights
        eprintln!("[SideBySideDiff] Collected {} left highlights, {} right highlights", 
            left_highlights.len(), right_highlights.len());
        for (i, (range, _, kind)) in right_highlights.iter().enumerate() {
            eprintln!("[SideBySideDiff]   Right highlight {}: rows {}..{}, kind={}", i, range.start, range.end, 
                match kind { HighlightKind::Deletion => "del", HighlightKind::Addition => "add", HighlightKind::Modification => "mod" });
        }
        
        // Apply highlighting to left editor
        self.left_editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let max_row = snapshot.max_point().row;  // Use MultiBuffer snapshot's max_row
            for (row_range, color, kind) in &left_highlights {
                if row_range.start < row_range.end && row_range.start <= max_row {
                    let end_row = (row_range.end).min(max_row + 1);
                    let adjusted_range = row_range.start..end_row;
                    apply_highlight!(editor, snapshot, adjusted_range, *color, kind, highlight_options, cx);
                }
            }
        });
        
        // Apply highlighting to right editor
        self.right_editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let max_row = snapshot.max_point().row;  // Use MultiBuffer snapshot's max_row
            eprintln!("[SideBySideDiff] Applying right highlights, max_row={} (new_buffer_max={})", 
                max_row, new_buffer_snapshot.max_point().row);
            for (row_range, color, kind) in &right_highlights {
                eprintln!("[SideBySideDiff]   Checking row_range {}..{}, condition: start<end={}, start<=max_row={}", 
                    row_range.start, row_range.end, 
                    row_range.start < row_range.end,
                    row_range.start <= max_row);
                if row_range.start < row_range.end && row_range.start <= max_row {
                    let end_row = (row_range.end).min(max_row + 1);
                    let adjusted_range = row_range.start..end_row;
                    eprintln!("[SideBySideDiff]   APPLYING highlight to rows {}..{}", adjusted_range.start, adjusted_range.end);
                    apply_highlight!(editor, snapshot, adjusted_range, *color, kind, highlight_options, cx);
                } else {
                    eprintln!("[SideBySideDiff]   SKIPPED highlight for rows {}..{}", row_range.start, row_range.end);
                }
            }
        });
        
        // Apply padding blocks
        for (row, count) in left_padding_ops {
            self.insert_padding_block_inner(&self.left_editor.clone(), row, count, true, cx);
        }
        for (row, count) in right_padding_ops {
            self.insert_padding_block_inner(&self.right_editor.clone(), row, count, false, cx);
        }
        
        // Apply word-level diff highlighting for left editor (deletions in base text)
        if !left_word_diffs.is_empty() {
            let word_deletion_color = cx.theme().colors().version_control_deleted;
            self.left_editor.update(cx, |editor, cx| {
                let snapshot = editor.buffer().read(cx).snapshot(cx);
                let mut word_ranges: Vec<std::ops::Range<multi_buffer::Anchor>> = Vec::new();
                
                for byte_range in &left_word_diffs {
                    // Convert byte offset in base_text to position in the left editor's buffer
                    // The left editor shows the old_buffer which was created from base_text
                    if byte_range.start < byte_range.end {
                        let start_offset = multi_buffer::MultiBufferOffset(byte_range.start);
                        let end_offset = multi_buffer::MultiBufferOffset(byte_range.end);
                        let start = snapshot.anchor_after(snapshot.clip_offset(start_offset, text::Bias::Left));
                        let end = snapshot.anchor_before(snapshot.clip_offset(end_offset, text::Bias::Right));
                        word_ranges.push(start..end);
                    }
                }
                
                if !word_ranges.is_empty() {
                    eprintln!("[SideBySideDiff] Applying {} word deletion highlights to left editor", word_ranges.len());
                    editor.highlight_background::<WordDeletionHighlight>(
                        &word_ranges,
                        move |_, _| word_deletion_color,
                        cx,
                    );
                }
            });
        }
        
        // Apply word-level diff highlighting for right editor (additions in new buffer)
        eprintln!("[SideBySideDiff] Right word diffs: {} anchors", right_word_diffs.len());
        if !right_word_diffs.is_empty() {
            let word_addition_color = cx.theme().colors().version_control_added;
            self.right_editor.update(cx, |editor, cx| {
                let snapshot = editor.buffer().read(cx).snapshot(cx);
                let mut word_ranges: Vec<std::ops::Range<multi_buffer::Anchor>> = Vec::new();
                
                // Get the first excerpt id from the snapshot
                if let Some((excerpt_id, _, _)) = snapshot.excerpts().next() {
                    eprintln!("[SideBySideDiff] Found excerpt: {:?}", excerpt_id);
                    // right_word_diffs contains pairs of anchors (start, end)
                    let mut i = 0;
                    while i + 1 < right_word_diffs.len() {
                        let start_text_anchor = right_word_diffs[i];
                        let end_text_anchor = right_word_diffs[i + 1];
                        
                        // Convert text::Anchor to multi_buffer::Anchor
                        let mb_start_opt = snapshot.anchor_in_excerpt(excerpt_id, start_text_anchor);
                        let mb_end_opt = snapshot.anchor_in_excerpt(excerpt_id, end_text_anchor);
                        eprintln!("[SideBySideDiff]   Word diff {}: start_ok={}, end_ok={}", i/2, mb_start_opt.is_some(), mb_end_opt.is_some());
                        if let (Some(mb_start), Some(mb_end)) = (mb_start_opt, mb_end_opt) {
                            word_ranges.push(mb_start..mb_end);
                        }
                        i += 2;
                    }
                } else {
                    eprintln!("[SideBySideDiff] No excerpts found in right editor snapshot");
                }
                
                eprintln!("[SideBySideDiff] word_ranges collected: {}", word_ranges.len());
                if !word_ranges.is_empty() {
                    eprintln!("[SideBySideDiff] Applying {} word addition highlights to right editor", word_ranges.len());
                    editor.highlight_background::<WordAdditionHighlight>(
                        &word_ranges,
                        move |_, _| word_addition_color,
                        cx,
                    );
                }
            });
        }
    }

    /// Insert a padding block (empty lines) to align editors
    fn insert_padding_block_inner(
        &mut self,
        editor: &Entity<Editor>,
        at_row: u32,
        line_count: u32,
        is_left: bool,
        cx: &mut Context<Self>,
    ) {
        if line_count == 0 {
            return;
        }

        let block_ids = editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let max_point = snapshot.max_point();

            // Clamp row to valid range
            let row = at_row.min(max_point.row);
            let anchor = snapshot.anchor_before(Point::new(row, 0));

            editor.insert_blocks(
                [BlockProperties {
                    placement: BlockPlacement::Above(anchor),
                    height: Some(line_count),
                    style: BlockStyle::Fixed,
                    render: Arc::new(move |bx| {
                        // Render empty padding lines with subtle background
                        let theme = bx.theme();
                        div()
                            .id(bx.block_id)
                            .w_full()
                            .h(bx.line_height * line_count as f32)
                            .bg(theme.colors().editor_background.opacity(0.3))
                            .into_any_element()
                    }),
                    priority: 0,
                }],
                None,
                cx,
            )
        });
        
        // Track the block IDs for later removal
        if is_left {
            self.left_alignment_blocks.extend(block_ids);
        } else {
            self.right_alignment_blocks.extend(block_ids);
        }
    }

    /// Insert a padding block (empty lines) to align editors
    #[allow(dead_code)]
    fn insert_padding_block(
        &mut self,
        editor: &Entity<Editor>,
        at_row: u32,
        line_count: u32,
        cx: &mut Context<Self>,
    ) {
        if line_count == 0 {
            return;
        }

        editor.update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let max_point = snapshot.max_point();

            // Clamp row to valid range
            let row = at_row.min(max_point.row);
            let anchor = snapshot.anchor_before(Point::new(row, 0));

            let block_ids = editor.insert_blocks(
                [BlockProperties {
                    placement: BlockPlacement::Above(anchor),
                    height: Some(line_count),
                    style: BlockStyle::Fixed,
                    render: Arc::new(move |cx| {
                        // Render empty padding lines with subtle background
                        let theme = cx.theme();
                        div()
                            .id(cx.block_id)
                            .w_full()
                            .h(cx.line_height * line_count as f32)
                            .bg(theme.colors().editor_background.opacity(0.5))
                            .into_any_element()
                    }),
                    priority: 0,
                }],
                None,
                cx,
            );

            // Track the block ID (we'd need to store these properly)
            // For now, we just insert them
            drop(block_ids);
        });
    }

    /// Clear all alignment blocks and row highlights
    fn clear_alignment_blocks(&mut self, cx: &mut Context<Self>) {
        // Take the block IDs out first before any mutable borrows
        let left_block_ids = std::mem::take(&mut self.left_alignment_blocks);
        let right_block_ids = std::mem::take(&mut self.right_alignment_blocks);
        
        // Remove existing blocks and highlights from left editor
        self.left_editor.update(cx, |editor, cx| {
            // Clear all row highlight types
            editor.clear_row_highlights::<DeletionHighlight>();
            editor.clear_row_highlights::<AdditionHighlight>();
            editor.clear_row_highlights::<ModificationHighlight>();
            // Clear word-level highlights
            editor.clear_background_highlights::<WordDeletionHighlight>(cx);
            
            // Then remove blocks
            if !left_block_ids.is_empty() {
                editor.remove_blocks(left_block_ids.into_iter().collect(), None, cx);
            }
        });
        
        // Remove existing blocks and highlights from right editor
        self.right_editor.update(cx, |editor, cx| {
            // Clear all row highlight types
            editor.clear_row_highlights::<DeletionHighlight>();
            editor.clear_row_highlights::<AdditionHighlight>();
            editor.clear_row_highlights::<ModificationHighlight>();
            // Clear word-level highlights
            editor.clear_background_highlights::<WordAdditionHighlight>(cx);
            
            // Then remove blocks
            if !right_block_ids.is_empty() {
                editor.remove_blocks(right_block_ids.into_iter().collect(), None, cx);
            }
        });
    }

    /// Calculate whether there are hunks before/after the current cursor position
    /// Returns (has_prev_hunk, has_next_hunk)
    fn hunk_navigation_state(&self, cx: &App) -> (bool, bool) {
        let new_buffer_snapshot = self.new_buffer.read(cx).snapshot();
        let diff_snapshot = self.diff.read(cx).snapshot(cx);
        let hunks: Vec<_> = diff_snapshot.hunks(&new_buffer_snapshot).collect();

        if hunks.is_empty() {
            return (false, false);
        }

        // Get current cursor position in the right editor (new buffer)
        // Use newest_anchor and convert to point via multibuffer snapshot
        let right_editor = self.right_editor.read(cx);
        let mb_snapshot = right_editor.buffer().read(cx).snapshot(cx);
        let newest_anchor = right_editor.selections.newest_anchor();
        let current_row = newest_anchor.head().to_point(&mb_snapshot).row;

        let has_prev = hunks.iter().any(|hunk| hunk.range.start.row < current_row);
        let has_next = hunks.iter().any(|hunk| hunk.range.start.row > current_row);

        (has_prev, has_next)
    }

    /// Navigate to the next hunk in the diff
    fn go_to_next_hunk(&mut self, _: &GoToNextHunk, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_to_hunk(true, window, cx);
    }

    /// Navigate to the previous hunk in the diff
    fn go_to_previous_hunk(&mut self, _: &GoToPreviousHunk, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_to_hunk(false, window, cx);
    }

    /// Navigate to the next or previous hunk
    /// 
    /// This method:
    /// 1. Finds all hunks in the diff
    /// 2. Determines the current position based on the right editor's cursor
    /// 3. Finds the target hunk (next or previous)
    /// 4. Scrolls the target hunk to a position slightly above center (for better UX)
    /// 5. Places the cursor at the beginning of the hunk's first line
    /// 6. Syncs the scroll position to the left editor
    fn navigate_to_hunk(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Get hunks from the diff
        let new_buffer_snapshot = self.new_buffer.read(cx).snapshot();
        let diff_snapshot = self.diff.read(cx).snapshot(cx);
        let hunks: Vec<_> = diff_snapshot.hunks(&new_buffer_snapshot).collect();

        if hunks.is_empty() {
            return;
        }

        // Get current cursor position in the right editor (new buffer)
        let current_row = self.right_editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let selection = editor.selections.newest::<Point>(&snapshot);
            selection.head().row
        });

        // Find the target hunk index
        let target_index = if next {
            // Find the first hunk that starts after the current row
            hunks.iter().position(|hunk| hunk.range.start.row > current_row)
                .or_else(|| {
                    // If no hunk after current, wrap to first
                    if !hunks.is_empty() { Some(0) } else { None }
                })
        } else {
            // Find the last hunk that starts before the current row
            let mut found_idx = None;
            for (i, hunk) in hunks.iter().enumerate().rev() {
                if hunk.range.start.row < current_row {
                    found_idx = Some(i);
                    break;
                }
            }
            found_idx.or_else(|| {
                // If no hunk before current, wrap to last
                if !hunks.is_empty() { Some(hunks.len() - 1) } else { None }
            })
        };

        let Some(target_index) = target_index else {
            return;
        };

        let target_hunk = &hunks[target_index];
        let target_row = target_hunk.range.start.row;

        // Navigate to the target hunk in the right editor
        // Use Autoscroll::top_relative to position the hunk slightly above center
        // This provides a better user experience as the user can see more context below
        self.right_editor.update(cx, |editor, cx| {
            let destination = Point::new(target_row, 0);
            
            // Unfold the destination if needed
            editor.unfold_ranges(&[destination..destination], false, false, cx);
            
            // Move cursor to the hunk's first line and scroll with smooth animation feel
            // Using top_relative(5) to position the hunk ~5 lines from top (above center)
            editor.change_selections(
                editor::SelectionEffects::scroll(Autoscroll::top_relative(5)),
                window,
                cx,
                |s| {
                    s.select_ranges([destination..destination]);
                },
            );
        });

        // Focus the right editor to ensure cursor is visible
        self.right_editor.update(cx, |_editor, cx| {
            cx.focus_self(window);
        });

        // Sync scroll position to left editor after a short delay
        // to ensure the right editor has finished scrolling
        let left_editor = self.left_editor.clone();
        let right_editor = self.right_editor.clone();
        window.defer(cx, move |window, cx| {
            let scroll_position = right_editor.update(cx, |editor, cx| {
                editor.scroll_position(cx)
            });
            left_editor.update(cx, |editor, cx| {
                editor.set_scroll_position(scroll_position, window, cx);
            });
        });
    }

    /// Render the diff gutter with change indicators
    #[allow(dead_code)]
    fn render_diff_gutter(&self, _cx: &App) -> AnyElement {
        // Placeholder for diff connector lines
        div()
            .w(px(40.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(2.))
                    .h_full()
                    .bg(gpui::rgb(0x444444))
            )
            .into_any_element()
    }
}

impl Render for SideBySideDiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        // Get the filename for display
        let filename = self.path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let left_title = format!("{} @ {} (Read-only)", filename, self.base_label);
        let right_title = format!("{} (Worktree)", filename);
        
        div()
            .id("side-by-side-diff-view")
            .track_focus(&self.focus_handle)
            .key_context("SideBySideDiffView")
            .on_action(cx.listener(Self::go_to_next_hunk))
            .on_action(cx.listener(Self::go_to_previous_hunk))
            .size_full()
            .flex()
            .flex_row()
            .bg(theme.colors().editor_background)
            // Left panel header + editor
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .border_r_1()
                    .border_color(theme.colors().border)
                    // Header
                    .child(
                        div()
                            .h(px(32.))
                            .px_2()
                            .flex()
                            .items_center()
                            .bg(theme.colors().title_bar_background)
                            .border_b_1()
                            .border_color(theme.colors().border)
                            .child(
                                Label::new(left_title)
                                    .size(ui::LabelSize::Small)
                                    .color(Color::Muted),
                            )
                    )
                    // Left editor
                    .child(
                        div()
                            .flex_1()
                            .child(self.left_editor.clone())
                    )
            )
            // Center gutter with connectors (optional)
            // .child(self.render_diff_gutter(cx))
            // Right panel header + editor
            .child({
                // Calculate hunk navigation state
                let (has_prev, has_next) = self.hunk_navigation_state(cx);
                let focus_handle = self.focus_handle.clone();
                
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    // Header with navigation buttons
                    .child(
                        div()
                            .h(px(32.))
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .bg(theme.colors().title_bar_background)
                            .border_b_1()
                            .border_color(theme.colors().border)
                            .child(
                                Label::new(right_title.clone())
                                    .size(ui::LabelSize::Small)
                                    .color(Color::Accent),
                            )
                            // Navigation buttons
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(
                                        IconButton::new("prev-hunk", IconName::ArrowUp)
                                            .icon_size(ui::IconSize::Small)
                                            .tooltip(Tooltip::for_action_title_in(
                                                "Previous Hunk",
                                                &GoToPreviousHunk,
                                                &focus_handle,
                                            ))
                                            .disabled(!has_prev)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.navigate_to_hunk(false, window, cx);
                                            })),
                                    )
                                    .child(
                                        IconButton::new("next-hunk", IconName::ArrowDown)
                                            .icon_size(ui::IconSize::Small)
                                            .tooltip(Tooltip::for_action_title_in(
                                                "Next Hunk",
                                                &GoToNextHunk,
                                                &focus_handle,
                                            ))
                                            .disabled(!has_next)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.navigate_to_hunk(true, window, cx);
                                            })),
                                    )
                            )
                    )
                    // Right editor
                    .child(
                        div()
                            .flex_1()
                            .child(self.right_editor.clone())
                    )
            })
    }
}

impl Focusable for SideBySideDiffView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<ItemEvent> for SideBySideDiffView {}

impl Item for SideBySideDiffView {
    type Event = ItemEvent;

    fn tab_content(&self, params: TabContentParams, _window: &Window, _cx: &App) -> AnyElement {
        let label = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Diff".to_string());

        Label::new(format!("⇔ {}", label))
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Diff).color(Color::Muted))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        let label = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Diff".to_string());
        format!("⇔ {}", label).into()
    }

    fn to_item_events(event: &Self::Event, mut f: impl FnMut(ItemEvent)) {
        f(*event)
    }

    fn deactivated(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn navigate(&mut self, _: Box<dyn Any>, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        false
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(format!("Diff: {}", self.path.display()).into())
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.right_editor.read(cx).is_dirty(cx)
    }

    fn has_conflict(&self, cx: &App) -> bool {
        self.right_editor.read(cx).has_conflict(cx)
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
        self.right_editor.update(cx, |editor, cx| {
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
        self.right_editor.update(cx, |editor, cx| {
            editor.save_as(project, path, window, cx)
        })
    }

    fn reload(
        &mut self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.right_editor.update(cx, |editor, cx| {
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
        self.right_editor.update(cx, |editor, _cx| {
            editor.set_nav_history(Some(nav_history));
        });
    }
}

use project::ProjectPath;
