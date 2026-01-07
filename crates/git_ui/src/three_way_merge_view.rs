use editor::{
    display_map::{BlockContext, CustomBlockId},
    Editor, ExcerptId,
};
use gpui::{
    Context, InteractiveElement as _, ParentElement as _, Styled,
    WeakEntity,
};
use language::OffsetRangeExt;
use project::ConflictRegion;
use ui::{prelude::*, ActiveTheme};

/// 3-way merge view 组件
/// 用于显示冲突的三方内容（Base/Ours/Theirs）以及解决冲突按钮
#[allow(dead_code)]
pub struct ThreeWayMergeView {
    editor: WeakEntity<Editor>,
    conflict: ConflictRegion,
    excerpt_id: ExcerptId,
    blocks: Vec<CustomBlockId>,
}

impl ThreeWayMergeView {
    pub fn new(
        editor: WeakEntity<Editor>,
        conflict: ConflictRegion,
        excerpt_id: ExcerptId,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            editor,
            conflict,
            excerpt_id,
            blocks: Vec::new(),
        }
    }

    pub fn render_three_way_view(
        conflict: &ConflictRegion,
        excerpt_id: ExcerptId,
        editor: WeakEntity<Editor>,
        buffer_text: &language::BufferSnapshot,
        cx: &mut BlockContext,
    ) -> AnyElement {
        // IntelliJ-style 3-way merge layout:
        // +---------------+---------------+---------------+
        // |     Base      |     Ours      |    Theirs     |
        // | (Common)      |   (HEAD)      |  (MERGE_HEAD) |
        // +---------------+---------------+---------------+

        // Get text from buffer for each section
        let base_text = if let Some(base_range) = &conflict.base {
            
            buffer_text.text_for_range(base_range.to_offset(buffer_text)).collect::<String>()
        } else {
            conflict.base_text.clone().unwrap_or_else(|| "(Base version not available)".to_string())
        };
        
        let ours_text = {
            
            buffer_text.text_for_range(conflict.ours.to_offset(buffer_text)).collect::<String>()
        };
        let theirs_text = {
            
            buffer_text.text_for_range(conflict.theirs.to_offset(buffer_text)).collect::<String>()
        };

        let theme = cx.theme();
        let base_bg = theme.colors().editor_document_highlight_read_background;
        let ours_bg = theme.colors().version_control_conflict_marker_ours;
        let theirs_bg = theme.colors().version_control_conflict_marker_theirs;

        v_flex()
            .id(cx.block_id)
            .w_full()
            .bg(theme.colors().editor_background)
            .border_1()
            .border_color(theme.colors().border)
            .rounded_md()
            .p_2()
            .gap_2()
            .child(
                // Header row
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(
                                Label::new("Base (Common Ancestor)")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(
                                Label::new(format!("Ours ({})", conflict.ours_branch_name))
                                    .size(LabelSize::Small)
                                    .color(Color::Accent),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(
                                Label::new(format!("Theirs ({})", conflict.theirs_branch_name))
                                    .size(LabelSize::Small)
                                    .color(Color::Conflict),
                            ),
                    ),
            )
            .child(
                // Content row with three columns
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        // Base column
                        div()
                            .flex_1()
                            .bg(base_bg)
                            .border_1()
                            .border_color(theme.colors().border_variant)
                            .rounded_sm()
                            .p_2()
                            .max_h(cx.line_height * 20.)
                            .child(Self::render_text_content(&base_text, theme)),
                    )
                    .child(
                        // Ours column
                        div()
                            .flex_1()
                            .bg(ours_bg)
                            .border_1()
                            .border_color(theme.colors().border_variant)
                            .rounded_sm()
                            .p_2()
                            .max_h(cx.line_height * 20.)
                            .child(Self::render_text_content(&ours_text, theme)),
                    )
                    .child(
                        // Theirs column
                        div()
                            .flex_1()
                            .bg(theirs_bg)
                            .border_1()
                            .border_color(theme.colors().border_variant)
                            .rounded_sm()
                            .p_2()
                            .max_h(cx.line_height * 20.)
                            .child(Self::render_text_content(&theirs_text, theme)),
                    ),
            )
            .child(
                // Action buttons row
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new(
                            "accept_base",
                            "Accept Base",
                        )
                        .label_size(LabelSize::Small)
                        .on_click({
                            let editor = editor.clone();
                            let conflict = conflict.clone();
                            let base = conflict.base.clone();
                            move |_, window, cx| {
                                if let Some(base_range) = base.clone() {
                                    crate::conflict_view::resolve_conflict(
                                        editor.clone(),
                                        excerpt_id,
                                        conflict.clone(),
                                        vec![base_range],
                                        window,
                                        cx,
                                    )
                                    .detach();
                                }
                            }
                        }),
                    )
                    .child(
                        Button::new(
                            "accept_ours",
                            format!("Accept {}", conflict.ours_branch_name),
                        )
                        .label_size(LabelSize::Small)
                        .on_click({
                            let editor = editor.clone();
                            let conflict = conflict.clone();
                            let ours = conflict.ours.clone();
                            move |_, window, cx| {
                                crate::conflict_view::resolve_conflict(
                                    editor.clone(),
                                    excerpt_id,
                                    conflict.clone(),
                                    vec![ours.clone()],
                                    window,
                                    cx,
                                )
                                .detach();
                            }
                        }),
                    )
                    .child(
                        Button::new(
                            "accept_theirs",
                            format!("Accept {}", conflict.theirs_branch_name),
                        )
                        .label_size(LabelSize::Small)
                        .on_click({
                            let editor = editor.clone();
                            let conflict = conflict.clone();
                            let theirs = conflict.theirs.clone();
                            move |_, window, cx| {
                                crate::conflict_view::resolve_conflict(
                                    editor.clone(),
                                    excerpt_id,
                                    conflict.clone(),
                                    vec![theirs.clone()],
                                    window,
                                    cx,
                                )
                                .detach();
                            }
                        }),
                    )
                    .child(
                        Button::new(
                            "accept_both",
                            "Accept Both",
                        )
                        .label_size(LabelSize::Small)
                        .on_click({
                            let conflict = conflict.clone();
                            let ours = conflict.ours.clone();
                            let theirs = conflict.theirs.clone();
                            move |_, window, cx| {
                                crate::conflict_view::resolve_conflict(
                                    editor.clone(),
                                    excerpt_id,
                                    conflict.clone(),
                                    vec![ours.clone(), theirs.clone()],
                                    window,
                                    cx,
                                )
                                .detach();
                            }
                        }),
                    ),
            )
            .into_any()
    }

    fn render_text_content(text: &str, theme: &theme::Theme) -> AnyElement {
        v_flex()
            .gap_px()
            .children(text.lines().map(|line| {
                div()
                    .text_xs()
                    .text_color(theme.colors().editor_foreground)
                    .font_family("monospace")
                    .child(line.to_string())
            }))
            .into_any()
    }
}
