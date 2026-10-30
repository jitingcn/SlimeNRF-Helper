use super::{Workbench, home::HomeView};
use crate::components::group_panel;
use crate::features::commands::{
    editor::CommandEditorState,
    library::{CommandLibrary, LibraryCompletion},
};
use crate::{
    application::shortcuts::{custom_step_summary, target_summary},
    components::{action_card, context_button},
    features::commands::{
        batch::RunId,
        model::{
            CommandCardIcon, DraftField, MoveDirection, SaveError, SavedCommandId, TargetMode,
            ValidationReason,
        },
        store::ShortcutEdit,
    },
    i18n::tr,
};
use futures_util::future::Shared;
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, Textarea},
};
use gpui_kit::{
    AnyElement, Context, Div, FontWeight, SharedString, Task, Window, div, prelude::*, px,
};
use std::collections::BTreeSet;

#[derive(Default)]
pub(in crate::application) struct SavedCommandsState {
    library: CommandLibrary,
    view: SavedCommandView,
    delete: Option<SavedCommandId>,
    expanded_runs: BTreeSet<RunId>,
}

#[derive(Default)]
enum SavedCommandView {
    #[default]
    Library,
    Editor(CommandEditorState),
    Targets(CommandTargetState),
    Review,
}

struct CommandTargetState {
    id: SavedCommandId,
    receivers: BTreeSet<String>,
}

impl SavedCommandsState {
    fn finish_store_job(&mut self) {
        match self.library.finish() {
            LibraryCompletion::Loaded | LibraryCompletion::Failed => {}
            LibraryCompletion::Changed(saved_id) => {
                self.delete = None;
                if saved_id.is_some() {
                    self.view = SavedCommandView::Library;
                }
            }
            LibraryCompletion::Validation(error) => {
                if let Some(editor) = self.editor_mut() {
                    editor.set_validation(error);
                } else {
                    self.library.set_error(validation_message(&error));
                }
            }
        }
    }

    fn open_library(&mut self) {
        if matches!(self.view, SavedCommandView::Review) {
            self.view = SavedCommandView::Library;
        }
        self.delete = None;
    }

    fn edit(
        &mut self,
        id: Option<SavedCommandId>,
        window: &mut Window,
        cx: &mut Context<Workbench>,
    ) -> bool {
        if self.library.busy() || !self.library.has_store() {
            return false;
        }
        let saved = id.and_then(|id| self.library.card(id));
        if id.is_some() && saved.is_none() {
            return false;
        }
        self.view = SavedCommandView::Editor(CommandEditorState::new(
            saved,
            window,
            cx,
            Workbench::saved_editor_changed,
        ));
        self.library.clear_error();
        if let Some(editor) = self.editor() {
            editor.focus(window, cx);
        }
        true
    }

    fn editor(&self) -> Option<&CommandEditorState> {
        match &self.view {
            SavedCommandView::Editor(editor) => Some(editor),
            _ => None,
        }
    }

    fn editor_mut(&mut self) -> Option<&mut CommandEditorState> {
        match &mut self.view {
            SavedCommandView::Editor(editor) => Some(editor),
            _ => None,
        }
    }

    fn editor_changed(&mut self) {
        if let Some(editor) = self.editor_mut() {
            editor.changed();
        }
    }

    fn cancel_editor(&mut self) {
        self.show_library();
        self.library.clear_error();
    }

    fn show_library(&mut self) {
        self.view = SavedCommandView::Library;
    }

    fn choose_targets(&mut self, id: SavedCommandId) {
        self.view = SavedCommandView::Targets(CommandTargetState {
            id,
            receivers: BTreeSet::new(),
        });
    }

    fn selected_command(
        &self,
    ) -> Option<(
        crate::features::commands::model::SavedCommandCard,
        BTreeSet<String>,
    )> {
        let SavedCommandView::Targets(targets) = &self.view else {
            return None;
        };
        Some((
            self.library.card(targets.id)?.clone(),
            targets.receivers.clone(),
        ))
    }

    fn select_receivers(&mut self, receivers: BTreeSet<String>) {
        if let SavedCommandView::Targets(targets) = &mut self.view {
            targets.receivers = receivers;
        }
    }

    fn clear_receivers(&mut self) {
        if let SavedCommandView::Targets(targets) = &mut self.view {
            targets.receivers.clear();
        }
    }

    fn set_receiver_selected(&mut self, key: &str, selected: bool) {
        if let SavedCommandView::Targets(targets) = &mut self.view {
            if selected {
                targets.receivers.insert(key.to_owned());
            } else {
                targets.receivers.remove(key);
            }
        }
    }

    fn review_prepared(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.view = SavedCommandView::Review;
                self.library.clear_error();
            }
            Err(error) => self.library.set_error(error),
        }
    }

    fn request_delete(&mut self, id: SavedCommandId) {
        self.delete = Some(id);
    }

    fn cancel_delete(&mut self) {
        self.delete = None;
    }

    fn toggle_run_details(&mut self, id: RunId, retained: &BTreeSet<RunId>) {
        self.expanded_runs.retain(|id| retained.contains(id));
        if !self.expanded_runs.remove(&id) {
            self.expanded_runs.insert(id);
        }
    }

    fn dismissed_run(&mut self, id: RunId) {
        self.expanded_runs.remove(&id);
    }
}

impl Workbench {
    pub(in crate::application) fn initialize_saved_commands(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        if let Some(join) = self
            .saved_commands
            .library
            .initialize(cx.background_executor())
        {
            self.watch_saved_store_job(join, cx);
        }
    }

    fn reload_saved_commands(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        if let Some(join) = self.saved_commands.library.reload(cx.background_executor()) {
            self.watch_saved_store_job(join, cx);
        }
    }

    pub(in crate::application) fn saved_commands_busy(&self) -> bool {
        self.saved_commands.library.busy()
    }

    pub(in crate::application) fn saved_commands_shutdown(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        self.saved_commands
            .library
            .shutdown(cx.background_executor())
    }

    fn watch_saved_store_job(&mut self, join: Shared<Task<()>>, cx: &mut Context<Self>) {
        let completion = cx.spawn(async move |this, cx| {
            join.await;
            let _ = this.update(cx, |this, cx| {
                this.saved_commands.finish_store_job();
                cx.notify();
            });
        });
        self.saved_commands.library.retain_completion(completion);
        cx.notify();
    }

    fn apply_saved_edit(&mut self, edit: ShortcutEdit, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let invalidated_id = match &edit {
            ShortcutEdit::Save(draft) => draft.id,
            ShortcutEdit::Remove(id) => Some(*id),
            _ => None,
        };
        let Some(join) = self
            .saved_commands
            .library
            .apply(edit, cx.background_executor())
        else {
            return;
        };
        if let Some(id) = invalidated_id {
            self.invalidate_saved_command_review(id);
        }
        self.watch_saved_store_job(join, cx);
    }

    pub(super) fn open_saved_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.show_home_view(HomeView::Commands, window, cx) {
            return;
        }
        self.cancel_custom_review();
        self.saved_commands.open_library();
    }

    fn edit_saved_command(
        &mut self,
        id: Option<SavedCommandId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saved_commands.edit(id, window, cx) {
            self.cancel_custom_review();
            cx.notify();
        }
    }

    fn saved_editor_changed(&mut self, cx: &mut Context<Self>) {
        self.saved_commands.editor_changed();
        self.cancel_custom_review();
        cx.notify();
    }

    fn save_saved_command(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.saved_commands.editor() else {
            return;
        };
        let draft = editor.draft(cx);
        self.apply_saved_edit(ShortcutEdit::Save(draft), cx);
    }

    fn choose_saved_targets(
        &mut self,
        id: SavedCommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saved_commands_busy() || !self.show_home_view(HomeView::Commands, window, cx) {
            return;
        }
        self.cancel_custom_review();
        self.saved_commands.choose_targets(id);
    }

    fn review_selected_saved_command(&mut self, cx: &mut Context<Self>) {
        let Some((card, receivers)) = self.saved_commands.selected_command() else {
            return;
        };
        let result = self.prepare_saved_command(card, receivers, cx);
        self.saved_commands.review_prepared(result);
        cx.notify();
    }

    pub(super) fn has_pinned_saved_commands(&self) -> bool {
        self.saved_commands.library.has_pinned_cards()
    }

    pub(super) fn saved_home_commands(&self, cx: &mut Context<Self>) -> Div {
        let mut section = div().w_full().flex().flex_col().min_w_0().gap_2();
        let Some(snapshot) = self.saved_commands.library.snapshot() else {
            return section;
        };
        section = section.child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr("home.section.commands")),
        );
        for card in snapshot.cards().iter().filter(|card| card.pinned) {
            let id = card.id;
            let mut content = div().w_full().flex().items_start().gap_3().min_w_0();
            if let Some(icon) = card.icon {
                content = content.child(div().flex_shrink_0().child(Icon::new(icon_name(icon))));
            }
            let mut labels = div().flex().flex_1().flex_col().gap_1().min_w_0().child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(card.title.clone()),
            );
            if let Some(description) = &card.description {
                labels = labels.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(description.clone()),
                );
            }
            labels = labels.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(target_key(card.target_mode))),
            );
            section =
                section.child(
                    action_card(
                        SharedString::from(format!("saved-home:{}", id.get())),
                        !self.saved_commands_busy(),
                        cx,
                    )
                    .accessibility_label(card.title.clone())
                    .child(content.child(labels))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_saved_targets(id, window, cx)
                    })),
                );
        }
        section
    }

    pub(super) fn saved_commands_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.saved_commands_busy();
        let mut page = div().flex().flex_col().min_w_0().gap_4().child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    Button::new("saved-back-home")
                        .label(tr("shortcuts.action.back_home"))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.show_home_view(HomeView::Tasks, window, cx) {
                                this.cancel_custom_review();
                            }
                        })),
                )
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tr("shortcuts.library.title")),
                ),
        );
        if busy {
            page = page.child(div().text_sm().child(tr("shortcuts.status.saving")));
        }
        if let Some(error) = self.saved_commands.library.error() {
            page = page.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .whitespace_normal()
                    .child(error.to_owned()),
            );
            if !self.saved_commands.library.has_store() && !busy {
                page = page.child(
                    Button::new("saved-reload")
                        .label(tr("shortcuts.action.reload"))
                        .on_click(cx.listener(|this, _, _, cx| this.reload_saved_commands(cx))),
                );
            }
        }
        page = page.child(match &self.saved_commands.view {
            SavedCommandView::Library => self.saved_commands_library(cx),
            SavedCommandView::Editor(editor) => self.saved_command_editor(editor, cx),
            SavedCommandView::Targets(targets) => self.saved_command_target_picker(targets, cx),
            SavedCommandView::Review => div().into_any_element(),
        });
        page.child(self.custom_commands_panel(cx))
            .into_any_element()
    }

    fn saved_commands_library(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.saved_commands_busy() || !self.saved_commands.library.has_store();
        let mut list = div().flex().flex_col().gap_3().min_w_0().child(
            Button::new("saved-new")
                .label(tr("shortcuts.action.new"))
                .disabled(busy)
                .on_click(
                    cx.listener(|this, _, window, cx| this.edit_saved_command(None, window, cx)),
                ),
        );
        let Some(snapshot) = self.saved_commands.library.snapshot() else {
            return list.into_any_element();
        };
        if snapshot.cards().is_empty() {
            list = list.child(div().text_sm().child(tr("shortcuts.library.empty")));
        }
        for (index, card) in snapshot.cards().iter().enumerate() {
            let id = card.id;
            let prefix = format!("saved:{}", id.get());
            let mut row = group_panel(cx).gap_2();
            let mut heading = div().w_full().min_w_0().flex().items_start().gap_2();
            if let Some(icon) = card.icon {
                heading = heading.child(div().flex_shrink_0().child(Icon::new(icon_name(icon))));
            }
            row = row.child(
                heading.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_normal()
                        .child(card.title.clone()),
                ),
            );
            if let Some(description) = &card.description {
                row = row.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child(description.clone()),
                );
            }
            row = row.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} · {}",
                        tr(target_key(card.target_mode)),
                        rust_i18n::t!("shortcuts.review.step_count", count = card.lines.len())
                    )),
            );
            let mut controls = div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(
                    Button::new(SharedString::from(format!("{prefix}:run")))
                        .small()
                        .label(tr("shortcuts.action.run"))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.choose_saved_targets(id, window, cx)
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("{prefix}:edit")))
                        .small()
                        .label(tr("shortcuts.action.edit"))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.edit_saved_command(Some(id), window, cx)
                        })),
                );
            let pinned = card.pinned;
            controls = controls
                .child(
                    Button::new(SharedString::from(format!("{prefix}:pin")))
                        .small()
                        .label(tr(if pinned {
                            "shortcuts.action.unpin"
                        } else {
                            "shortcuts.action.pin"
                        }))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.apply_saved_edit(
                                ShortcutEdit::SetPinned {
                                    id,
                                    pinned: !pinned,
                                },
                                cx,
                            )
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("{prefix}:up")))
                        .small()
                        .label(tr("shortcuts.action.move_up"))
                        .disabled(busy || index == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.apply_saved_edit(
                                ShortcutEdit::Move {
                                    id,
                                    direction: MoveDirection::Up,
                                },
                                cx,
                            )
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("{prefix}:down")))
                        .small()
                        .label(tr("shortcuts.action.move_down"))
                        .disabled(busy || index + 1 == snapshot.cards().len())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.apply_saved_edit(
                                ShortcutEdit::Move {
                                    id,
                                    direction: MoveDirection::Down,
                                },
                                cx,
                            )
                        })),
                );
            if self.saved_commands.delete == Some(id) {
                controls = controls
                    .child(div().text_sm().child(tr("shortcuts.delete.confirm")))
                    .child(
                        Button::new(SharedString::from(format!("{prefix}:confirm-delete")))
                            .small()
                            .label(tr("shortcuts.action.delete"))
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.apply_saved_edit(ShortcutEdit::Remove(id), cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{prefix}:cancel-delete")))
                            .small()
                            .label(tr("common.action.cancel"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.saved_commands.cancel_delete();
                                cx.notify();
                            })),
                    );
            } else {
                controls = controls.child(
                    Button::new(SharedString::from(format!("{prefix}:delete")))
                        .small()
                        .label(tr("shortcuts.action.delete"))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.saved_commands.request_delete(id);
                            cx.notify();
                        })),
                );
            }
            list = list.child(row.child(controls));
        }
        list.into_any_element()
    }

    fn saved_command_editor(
        &self,
        editor: &CommandEditorState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.saved_commands_busy();
        let mut icons = div().flex().flex_wrap().items_center().gap_2();
        for icon in [
            None,
            Some(CommandCardIcon::Terminal),
            Some(CommandCardIcon::Usb),
            Some(CommandCardIcon::Wireless),
            Some(CommandCardIcon::Settings),
        ] {
            icons = icons.child(
                Button::new(SharedString::from(format!("saved-icon:{icon:?}")))
                    .label(tr(icon_key(icon)))
                    .map(|button| context_button(button, editor.icon() == icon, cx))
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(editor) = this.saved_commands.editor_mut() {
                            editor.set_icon(icon);
                        }
                        this.saved_editor_changed(cx);
                    })),
            );
        }
        let mut modes = div().flex().flex_wrap().gap_2();
        for mode in [
            TargetMode::ConnectedTerminals,
            TargetMode::ReceiverConsoles,
            TargetMode::ReceiverTrackers,
        ] {
            modes = modes.child(
                Button::new(SharedString::from(format!("saved-mode:{mode:?}")))
                    .label(tr(target_key(mode)))
                    .map(|button| context_button(button, editor.target_mode() == mode, cx))
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(editor) = this.saved_commands.editor_mut() {
                            editor.set_target_mode(mode);
                        }
                        this.saved_editor_changed(cx);
                    })),
            );
        }
        let mut form = group_panel(cx)
            .gap_3()
            .child(field_label("shortcuts.editor.title"))
            .child(
                Input::new(editor.title())
                    .aria_label(tr("shortcuts.editor.title"))
                    .disabled(busy),
            )
            .child(field_label("shortcuts.editor.description"))
            .child(
                Input::new(editor.description())
                    .aria_label(tr("shortcuts.editor.description"))
                    .disabled(busy),
            )
            .child(field_label("shortcuts.editor.icon"))
            .child(icons)
            .child(field_label("shortcuts.editor.target_mode"))
            .child(modes)
            .child(field_label("shortcuts.editor.commands"))
            .child(
                Textarea::new(editor.commands())
                    .aria_label(tr("shortcuts.editor.commands"))
                    .disabled(busy),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("shortcuts.editor.commands_hint")),
            )
            .child(
                Checkbox::new("saved-pin-editor")
                    .label(tr("shortcuts.action.pin"))
                    .checked(editor.is_pinned())
                    .disabled(busy)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let Some(editor) = this.saved_commands.editor_mut() {
                            editor.set_pinned(*checked);
                        }
                        this.saved_editor_changed(cx);
                    })),
            );
        if let Some(error) = editor.validation() {
            form = form.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(validation_message(error)),
            );
        }
        form.child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("saved-save")
                        .primary()
                        .label(tr("shortcuts.action.save"))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.save_saved_command(cx))),
                )
                .child(
                    Button::new("saved-cancel-edit")
                        .label(tr("common.action.cancel"))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.saved_commands.cancel_editor();
                            cx.notify();
                        })),
                ),
        )
        .into_any_element()
    }

    fn saved_command_target_picker(
        &self,
        targets: &CommandTargetState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(card) = self.saved_commands.library.card(targets.id) else {
            return div().into_any_element();
        };
        let choices = self.saved_command_targets(card.target_mode);
        let terminal_mode = card.target_mode == TargetMode::ConnectedTerminals;
        let eligible = choices
            .iter()
            .filter(|choice| choice.exclusion.is_none())
            .count();
        let selected = if terminal_mode {
            eligible
        } else {
            choices
                .iter()
                .filter(|choice| {
                    choice.exclusion.is_none() && targets.receivers.contains(&choice.key)
                })
                .count()
        };
        let needs_preparation = !terminal_mode
            && choices.iter().any(|choice| {
                targets.receivers.contains(&choice.key)
                    && !choice.connected
                    && choice.exclusion.is_none()
            });
        let mut panel = group_panel(cx)
            .gap_3()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(card.title.clone()),
            )
            .child(div().text_sm().child(tr(target_key(card.target_mode))))
            .child(
                div().text_sm().child(
                    rust_i18n::t!(
                        "shortcuts.target.selected_count",
                        selected = selected,
                        available = eligible
                    )
                    .to_string(),
                ),
            );
        if card.target_mode == TargetMode::ReceiverTrackers {
            panel = panel.child(div().text_sm().child(tr("shortcuts.target.all_trackers")));
        }
        if !terminal_mode {
            let keys: BTreeSet<_> = choices
                .iter()
                .filter(|choice| choice.exclusion.is_none() && choice.connected)
                .map(|choice| choice.key.clone())
                .collect();
            panel = panel.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("saved-select-connected")
                            .small()
                            .label(tr("shortcuts.action.select_all"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.cancel_custom_review();
                                this.saved_commands.select_receivers(keys.clone());
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("saved-clear-targets")
                            .small()
                            .label(tr("shortcuts.action.clear_selection"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_custom_review();
                                this.saved_commands.clear_receivers();
                                cx.notify();
                            })),
                    ),
            );
        }
        for choice in choices {
            let mut row = div().flex().flex_col().gap_1().min_w_0();
            let label = choice.port.as_ref().map_or_else(
                || choice.label.clone(),
                |port| format!("{} · {port}", choice.label),
            );
            if terminal_mode {
                row = row.child(div().text_sm().child(label));
            } else {
                let key = choice.key.clone();
                row = row.child(
                    Checkbox::new(SharedString::from(format!("saved-receiver:{}", choice.key)))
                        .label(label)
                        .checked(targets.receivers.contains(&choice.key))
                        .disabled(choice.exclusion.is_some())
                        .on_click(cx.listener(move |this, checked, _, cx| {
                            this.cancel_custom_review();
                            this.saved_commands.set_receiver_selected(&key, *checked);
                            cx.notify();
                        })),
                );
            }
            if let Some(reason) = choice.exclusion {
                row = row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(reason),
                );
            } else if !choice.connected {
                row = row.child(div().text_xs().child(tr("shortcuts.target.not_connected")));
            }
            panel = panel.child(row);
        }
        if eligible == 0 {
            panel = panel.child(div().text_sm().child(tr("shortcuts.target.none")));
        }
        panel
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("saved-review")
                            .primary()
                            .label(tr(if needs_preparation {
                                "shortcuts.action.prepare"
                            } else {
                                "shortcuts.action.review"
                            }))
                            .disabled(selected == 0)
                            .on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.review_selected_saved_command(cx)
                                }),
                            ),
                    )
                    .child(
                        Button::new("saved-targets-cancel")
                            .label(tr("common.action.cancel"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_custom_review();
                                this.saved_commands.show_library();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("saved-targets-devices")
                            .label(tr("shortcuts.action.open_devices"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.set_feature_page(
                                    crate::application::FeaturePage::DeviceSetup,
                                    window,
                                    cx,
                                ) {
                                    this.cancel_custom_review();
                                    this.page_focus.focus(window, cx);
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .into_any_element()
    }

    pub(in crate::application) fn custom_commands_panel(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut panel = div().flex().flex_col().min_w_0().gap_3();
        if self.custom_command_preparing() {
            panel = panel
                .child(div().text_sm().child(tr("shortcuts.status.preparing")))
                .child(
                    Button::new("custom-cancel-preparing")
                        .label(tr("common.action.cancel"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_custom_review();
                            this.saved_commands.show_library();
                            cx.notify();
                        })),
                );
        }
        if let Some(error) = self.custom_command_error() {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .whitespace_normal()
                    .child(error.to_owned()),
            );
        }
        if let Some(review) = self.custom_command_review() {
            let mut content = group_panel(cx)
                .gap_2()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!(
                            "{} · {}",
                            tr("shortcuts.review.title"),
                            review.title()
                        )),
                )
                .child(div().text_sm().child(tr(target_key(review.target_mode()))))
                .child(
                    div().text_sm().child(
                        rust_i18n::t!(
                            "shortcuts.review.target_count",
                            count = review.targets().len()
                        )
                        .to_string(),
                    ),
                );
            if review.has_unknown_steps() || review.has_known_destructive_steps() {
                content = content.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .text_sm()
                        .text_color(if review.has_known_destructive_steps() {
                            cx.theme().danger
                        } else {
                            cx.theme().warning
                        })
                        .child(tr(if review.has_known_destructive_steps() {
                            "shortcuts.review.risky_caution"
                        } else {
                            "shortcuts.review.caution"
                        })),
                );
            }
            for target in review.targets() {
                content = content.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_normal()
                        .child(format!("{} · {}", target.label, target.port)),
                );
                for (index, line) in target.lines.iter().enumerate() {
                    content = content.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_sm()
                            .whitespace_normal()
                            .child(format!("{}. {line}", index + 1)),
                    );
                }
            }
            for excluded in review.excluded() {
                content = content.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child(format!("{} · {}", excluded.label, excluded.reason)),
                );
            }
            content = content.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("custom-confirm")
                            .primary()
                            .label(tr("commands.action.confirm_send"))
                            .disabled(review.targets().is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.confirm_custom_command(cx).is_ok() {
                                    this.saved_commands.show_library();
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("custom-review-cancel")
                            .label(tr("common.action.cancel"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_custom_review();
                                this.saved_commands.show_library();
                                cx.notify();
                            })),
                    ),
            );
            panel = panel.child(content);
        }
        let mut history = div()
            .id("custom-command-history")
            .flex()
            .flex_col()
            .min_h_0()
            .gap_3()
            .max_h(px(240.))
            .overflow_y_scroll();
        for run in self.saved_command_runs() {
            let id = run.id();
            let expanded = self.saved_commands.expanded_runs.contains(&id);
            let mut result = group_panel(cx).gap_2().child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(run.title().to_owned()),
            );
            for target in run.targets() {
                result = result.child(div().text_sm().whitespace_normal().child(format!(
                    "{} · {} · {}",
                    target.label,
                    target.port,
                    target_summary(&target.steps)
                )));
                if expanded {
                    for (index, step) in target.steps.iter().enumerate() {
                        let message = custom_step_summary(step);
                        result = result.child(
                            div()
                                .text_xs()
                                .whitespace_normal()
                                .child(format!("{}. {message}", index + 1)),
                        );
                    }
                }
            }
            let mut actions = div().flex().flex_wrap().gap_2().child(
                Button::new(SharedString::from(format!("custom-details:{}", id.get())))
                    .small()
                    .label(tr(if expanded {
                        "shortcuts.result.hide_details"
                    } else {
                        "shortcuts.result.details"
                    }))
                    .tooltip(tr("shortcuts.result.transport_only"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let retained: BTreeSet<_> =
                            this.saved_command_runs().map(|run| run.id()).collect();
                        this.saved_commands.toggle_run_details(id, &retained);
                        cx.notify();
                    })),
            );
            if run.pending() {
                actions = actions.child(
                    Button::new(SharedString::from(format!(
                        "custom-cancel-run:{}",
                        id.get()
                    )))
                    .small()
                    .label(tr("shortcuts.action.stop"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cancel_custom_run(id);
                        cx.notify();
                    })),
                );
            }
            if !run.pending() {
                actions = actions.child(
                    Button::new(SharedString::from(format!(
                        "custom-dismiss-run:{}",
                        id.get()
                    )))
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .tooltip(tr("shortcuts.result.dismiss"))
                    .accessibility_label(tr("shortcuts.result.dismiss"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.dismiss_custom_run(id) {
                            this.saved_commands.dismissed_run(id);
                        }
                        cx.notify();
                    })),
                );
            }
            history = history.child(result.child(actions));
        }
        if self.saved_command_runs().next().is_some() {
            panel = panel.child(history);
        }
        panel.into_any_element()
    }
}

fn field_label(key: &str) -> Div {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .child(tr(key))
}

fn target_key(mode: TargetMode) -> &'static str {
    match mode {
        TargetMode::ConnectedTerminals => "shortcuts.target.connected_terminals",
        TargetMode::ReceiverConsoles => "shortcuts.target.receiver_consoles",
        TargetMode::ReceiverTrackers => "shortcuts.target.receiver_trackers",
    }
}

fn icon_name(icon: CommandCardIcon) -> IconName {
    match icon {
        CommandCardIcon::Terminal => IconName::SquareTerminal,
        CommandCardIcon::Usb => IconName::Usb,
        CommandCardIcon::Wireless => IconName::Wifi,
        CommandCardIcon::Settings => IconName::Settings2,
    }
}

fn icon_key(icon: Option<CommandCardIcon>) -> &'static str {
    match icon {
        None => "shortcuts.icon.none",
        Some(CommandCardIcon::Terminal) => "shortcuts.icon.terminal",
        Some(CommandCardIcon::Usb) => "shortcuts.icon.usb",
        Some(CommandCardIcon::Wireless) => "shortcuts.icon.wireless",
        Some(CommandCardIcon::Settings) => "shortcuts.icon.settings",
    }
}

fn validation_message(error: &SaveError) -> String {
    let key = match error.reason {
        ValidationReason::TitleRequired => "shortcuts.error.title_required",
        ValidationReason::TitleTooLong => "shortcuts.error.title_too_long",
        ValidationReason::DescriptionTooLong => "shortcuts.error.description_too_long",
        ValidationReason::CommandsRequired => "shortcuts.error.commands_required",
        ValidationReason::BlankLine => "shortcuts.error.blank_line",
        ValidationReason::InvalidLine => "shortcuts.error.invalid_line",
        ValidationReason::LineTooLong => "shortcuts.error.line_too_long",
        ValidationReason::TooManySteps => "shortcuts.error.too_many_steps",
        ValidationReason::MacroTooLarge => "shortcuts.error.macro_too_large",
        ValidationReason::LibraryFull => "shortcuts.error.library_full",
        ValidationReason::IdExhausted => "shortcuts.error.id_exhausted",
        ValidationReason::UnknownId => "shortcuts.error.unknown_id",
        ValidationReason::InvalidId
        | ValidationReason::DuplicateId
        | ValidationReason::InvalidNextId => "shortcuts.error.invalid_library",
    };
    let message = tr(key);
    if let Some(line) = error.line {
        return format!(
            "{} · {message}",
            rust_i18n::t!("shortcuts.review.line_number", step = line)
        );
    }
    match error.field {
        DraftField::Title => format!("{} · {message}", tr("shortcuts.editor.title")),
        DraftField::Description => format!("{} · {message}", tr("shortcuts.editor.description")),
        DraftField::Commands | DraftField::Library => message,
    }
}

#[cfg(test)]
#[path = "saved/tests.rs"]
mod tests;
