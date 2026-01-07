//! IntelliJ-style Three-Way Merge Editor
//!
//! This provides a true three-panel merge editor:
//! - Left panel: "Ours" version (current branch, editable)
//! - Center panel: "Result" (merged output, editable)
//! - Right panel: "Theirs" version (incoming branch, read-only)
//!
//! With synchronized scrolling and visual diff highlighting.

use anyhow::Result;
use editor::{Editor, EditorEvent, ExcerptRange};
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled, Subscription, Task,
    Window, div,
};
use language::{Buffer, Capability};
use multi_buffer::MultiBuffer;
use project::{ConflictRegion, Project, ProjectPath};
use std::{
    any::Any,
    cell::Cell,
    path::PathBuf,
};
use ui::{
    ActiveTheme, Button, Color, Icon, IconName, Label,
    LabelCommon as _, LabelSize, SharedString, prelude::*,
};
use workspace::{
    Item, ItemNavHistory,
    item::{ItemEvent, TabContentParams},
};

/// Three-way merge editor for resolving conflicts
pub struct ThreeWayMergeEditor {
    /// Left panel: "Ours" (current branch)
    ours_editor: Entity<Editor>,
    /// Center panel: "Result" (merged output)
    result_editor: Entity<Editor>,
    /// Right panel: "Theirs" (incoming changes)
    theirs_editor: Entity<Editor>,
    /// The conflict being resolved
    conflict: ConflictRegion,
    /// Path of the conflicting file
    path: PathBuf,
    /// Focus handle
    focus_handle: FocusHandle,
    /// Prevent recursive scroll sync
    is_syncing_scroll: Cell<bool>,
    /// Subscriptions for event handling
    _subscriptions: Vec<Subscription>,
}

impl ThreeWayMergeEditor {
    /// Create a new three-way merge editor
    pub fn new(
        ours_text: String,
        _base_text: Option<String>,
        theirs_text: String,
        result_buffer: Entity<Buffer>,
        conflict: ConflictRegion,
        path: PathBuf,
        project: Option<Entity<Project>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // Create "Ours" editor (left, read-only display of our version)
        let ours_buffer = cx.new(|cx| {
            Buffer::local(ours_text, cx)
        });
        let ours_multibuffer = cx.new(|cx| {
            let mut mb = MultiBuffer::new(Capability::ReadOnly);
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

        // Create "Theirs" editor (right, read-only display of their version)
        let theirs_buffer = cx.new(|cx| {
            Buffer::local(theirs_text, cx)
        });
        let theirs_multibuffer = cx.new(|cx| {
            let mut mb = MultiBuffer::new(Capability::ReadOnly);
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

        // Create "Result" editor (center, editable merged result)
        let result_multibuffer = cx.new(|cx| {
            let mut mb = MultiBuffer::new(Capability::ReadWrite);
            mb.push_excerpts(
                result_buffer.clone(),
                [ExcerptRange::new(text::Anchor::MIN..text::Anchor::MAX)],
                cx,
            );
            mb
        });
        let result_editor = cx.new(|cx| {
            let mut editor = Editor::for_multibuffer(
                result_multibuffer.clone(),
                project.clone(),
                window,
                cx,
            );
            editor.set_show_gutter(true, cx);
            editor
        });

        // Set up scroll synchronization between all three editors
        let mut subscriptions = Vec::new();

        // Ours -> sync others
        let result_for_ours = result_editor.clone();
        let theirs_for_ours = theirs_editor.clone();
        subscriptions.push(cx.subscribe_in(
            &ours_editor,
            window,
            move |this, _, event: &EditorEvent, window, cx| {
                if let EditorEvent::ScrollPositionChanged { local: true, autoscroll: false } = event {
                    if !this.is_syncing_scroll.get() {
                        this.is_syncing_scroll.set(true);
                        let pos = this.ours_editor.update(cx, |e, cx| e.scroll_position(cx));
                        result_for_ours.update(cx, |e, cx| { e.set_scroll_position(pos, window, cx); });
                        theirs_for_ours.update(cx, |e, cx| { e.set_scroll_position(pos, window, cx); });
                        this.is_syncing_scroll.set(false);
                    }
                }
            },
        ));

        // Result -> sync others
        let ours_for_result = ours_editor.clone();
        let theirs_for_result = theirs_editor.clone();
        subscriptions.push(cx.subscribe_in(
            &result_editor,
            window,
            move |this, _, event: &EditorEvent, window, cx| {
                if let EditorEvent::ScrollPositionChanged { local: true, autoscroll: false } = event {
                    if !this.is_syncing_scroll.get() {
                        this.is_syncing_scroll.set(true);
                        let pos = this.result_editor.update(cx, |e, cx| e.scroll_position(cx));
                        ours_for_result.update(cx, |e, cx| { e.set_scroll_position(pos, window, cx); });
                        theirs_for_result.update(cx, |e, cx| { e.set_scroll_position(pos, window, cx); });
                        this.is_syncing_scroll.set(false);
                    }
                }
            },
        ));

        // Theirs -> sync others
        let ours_for_theirs = ours_editor.clone();
        let result_for_theirs = result_editor.clone();
        subscriptions.push(cx.subscribe_in(
            &theirs_editor,
            window,
            move |this, _, event: &EditorEvent, window, cx| {
                if let EditorEvent::ScrollPositionChanged { local: true, autoscroll: false } = event {
                    if !this.is_syncing_scroll.get() {
                        this.is_syncing_scroll.set(true);
                        let pos = this.theirs_editor.update(cx, |e, cx| e.scroll_position(cx));
                        ours_for_theirs.update(cx, |e, cx| { e.set_scroll_position(pos, window, cx); });
                        result_for_theirs.update(cx, |e, cx| { e.set_scroll_position(pos, window, cx); });
                        this.is_syncing_scroll.set(false);
                    }
                }
            },
        ));

        Self {
            ours_editor,
            result_editor,
            theirs_editor,
            conflict,
            path,
            focus_handle,
            is_syncing_scroll: Cell::new(false),
            _subscriptions: subscriptions,
        }
    }

    /// Open a three-way merge editor for a conflicted file in the workspace
    pub fn open(
        conflict: ConflictRegion,
        result_buffer: Entity<Buffer>,
        path: PathBuf,
        project: Entity<Project>,
        workspace: &mut workspace::Workspace,
        window: &mut Window,
        cx: &mut Context<workspace::Workspace>,
    ) {
        // Extract text from conflict region or use stored stage texts
        let result_snapshot = result_buffer.read(cx).snapshot();
        
        let ours_text = conflict
            .ours_text
            .clone()
            .unwrap_or_else(|| {
                result_snapshot
                    .text_for_range(conflict.ours.clone())
                    .collect()
            });
        
        let theirs_text = conflict
            .theirs_text
            .clone()
            .unwrap_or_else(|| {
                result_snapshot
                    .text_for_range(conflict.theirs.clone())
                    .collect()
            });
        
        let view = cx.new(|cx| {
            Self::new(
                ours_text,
                conflict.base_text.clone(),
                theirs_text,
                result_buffer,
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

    /// Accept "Ours" version - copy our text to result
    fn accept_ours(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ours_text = self.ours_editor.read(cx).text(cx);
        self.result_editor.update(cx, |editor, cx| {
            editor.set_text(ours_text, window, cx);
        });
    }

    /// Accept "Theirs" version - copy their text to result
    fn accept_theirs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theirs_text = self.theirs_editor.read(cx).text(cx);
        self.result_editor.update(cx, |editor, cx| {
            editor.set_text(theirs_text, window, cx);
        });
    }

    /// Accept both versions (ours then theirs)
    fn accept_both(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ours_text = self.ours_editor.read(cx).text(cx);
        let theirs_text = self.theirs_editor.read(cx).text(cx);
        let combined = format!("{}\n{}", ours_text.trim_end(), theirs_text);
        self.result_editor.update(cx, |editor, cx| {
            editor.set_text(combined, window, cx);
        });
    }

    /// Render action buttons for conflict resolution
    fn render_action_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ours_name = self.conflict.ours_branch_name.clone();
        let theirs_name = self.conflict.theirs_branch_name.clone();

        div()
            .h(px(40.))
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(theme.colors().title_bar_background)
            .border_t_1()
            .border_color(theme.colors().border)
            .child(
                Button::new("accept-ours", format!("Accept {} (Ours)", ours_name))
                    .label_size(LabelSize::Small)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.accept_ours(window, cx);
                    })),
            )
            .child(
                Button::new("accept-both", "Accept Both")
                    .label_size(LabelSize::Small)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.accept_both(window, cx);
                    })),
            )
            .child(
                Button::new("accept-theirs", format!("Accept {} (Theirs)", theirs_name))
                    .label_size(LabelSize::Small)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.accept_theirs(window, cx);
                    })),
            )
    }
}

impl Render for ThreeWayMergeEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ours_name = self.conflict.ours_branch_name.clone();
        let theirs_name = self.conflict.theirs_branch_name.clone();

        div()
            .id("three-way-merge-editor")
            .track_focus(&self.focus_handle)
            .key_context("ThreeWayMergeEditor")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.colors().editor_background)
            // Main content: three editors side by side
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    // Left: Ours
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .border_r_1()
                            .border_color(theme.colors().border)
                            .child(
                                div()
                                    .h(px(28.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .bg(theme.colors().version_control_conflict_marker_ours)
                                    .child(
                                        Label::new(format!("{} (Ours)", ours_name))
                                            .size(LabelSize::Small)
                                            .color(Color::Default),
                                    )
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(self.ours_editor.clone())
                            )
                    )
                    // Center: Result
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .border_r_1()
                            .border_color(theme.colors().border)
                            .child(
                                div()
                                    .h(px(28.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .bg(theme.colors().title_bar_background)
                                    .child(
                                        Label::new("Result (Editable)")
                                            .size(LabelSize::Small)
                                            .color(Color::Accent),
                                    )
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(self.result_editor.clone())
                            )
                    )
                    // Right: Theirs
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .h(px(28.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .bg(theme.colors().version_control_conflict_marker_theirs)
                                    .child(
                                        Label::new(format!("{} (Theirs)", theirs_name))
                                            .size(LabelSize::Small)
                                            .color(Color::Default),
                                    )
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(self.theirs_editor.clone())
                            )
                    )
            )
            // Bottom action bar
            .child(self.render_action_buttons(cx))
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

        Label::new(format!("⚡ Merge: {}", label))
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
        format!("⚡ Merge: {}", label).into()
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
        self.result_editor.read(cx).is_dirty(cx)
    }

    fn has_conflict(&self, _cx: &App) -> bool {
        true // This view exists to resolve conflicts
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
        self.result_editor.update(cx, |editor, cx| {
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
        self.result_editor.update(cx, |editor, cx| {
            editor.save_as(project, path, window, cx)
        })
    }

    fn reload(
        &mut self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.result_editor.update(cx, |editor, cx| {
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
        self.result_editor.update(cx, |editor, _cx| {
            editor.set_nav_history(Some(nav_history));
        });
    }
}
