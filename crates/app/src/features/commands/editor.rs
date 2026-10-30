use crate::features::commands::model::{
    CommandCardIcon, SaveError, SavedCommandCard, SavedCommandDraft, SavedCommandId, TargetMode,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::{App, AppContext as _, Context, Entity, Subscription, Window};

pub(crate) struct CommandEditorState {
    id: Option<SavedCommandId>,
    title: Entity<InputState>,
    description: Entity<InputState>,
    commands: Entity<TextareaState>,
    icon: Option<CommandCardIcon>,
    pinned: bool,
    target_mode: TargetMode,
    validation: Option<SaveError>,
    _subscriptions: Vec<Subscription>,
}

impl CommandEditorState {
    pub(crate) fn new<T: 'static>(
        saved: Option<&SavedCommandCard>,
        window: &mut Window,
        cx: &mut Context<T>,
        on_change: fn(&mut T, &mut Context<T>),
    ) -> Self {
        let title = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(saved.map_or("", |card| card.title.as_str()), window, cx);
            input
        });
        let description = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(
                saved
                    .and_then(|card| card.description.as_deref())
                    .unwrap_or(""),
                window,
                cx,
            );
            input
        });
        let commands = cx.new(|cx| {
            let mut input = TextareaState::new(window, cx).auto_grow(3, 8);
            input.set_value(
                saved
                    .map(SavedCommandCard::commands_text)
                    .unwrap_or_default(),
                window,
                cx,
            );
            input
        });
        let title_subscription = cx.subscribe_in(&title, window, move |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                on_change(this, cx);
            }
        });
        let description_subscription =
            cx.subscribe_in(&description, window, move |this, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    on_change(this, cx);
                }
            });
        let commands_subscription =
            cx.subscribe_in(&commands, window, move |this, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    on_change(this, cx);
                }
            });
        Self {
            id: saved.map(|card| card.id),
            title,
            description,
            commands,
            icon: saved.and_then(|card| card.icon),
            pinned: saved.is_none_or(|card| card.pinned),
            target_mode: saved.map_or(TargetMode::ConnectedTerminals, |card| card.target_mode),
            validation: None,
            _subscriptions: vec![
                title_subscription,
                description_subscription,
                commands_subscription,
            ],
        }
    }

    pub(crate) fn draft(&self, cx: &App) -> SavedCommandDraft {
        SavedCommandDraft {
            id: self.id,
            title: self.title.read(cx).text().to_string(),
            description: self.description.read(cx).text().to_string(),
            commands_text: self.commands.read(cx).text().to_string(),
            icon: self.icon,
            pinned: self.pinned,
            target_mode: self.target_mode,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.title.update(cx, |input, cx| input.focus(window, cx));
    }

    pub(crate) fn title(&self) -> &Entity<InputState> {
        &self.title
    }

    pub(crate) fn description(&self) -> &Entity<InputState> {
        &self.description
    }

    pub(crate) fn commands(&self) -> &Entity<TextareaState> {
        &self.commands
    }

    pub(crate) fn icon(&self) -> Option<CommandCardIcon> {
        self.icon
    }

    pub(crate) fn set_icon(&mut self, icon: Option<CommandCardIcon>) {
        self.icon = icon;
        self.changed();
    }

    pub(crate) fn is_pinned(&self) -> bool {
        self.pinned
    }

    pub(crate) fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
        self.changed();
    }

    pub(crate) fn target_mode(&self) -> TargetMode {
        self.target_mode
    }

    pub(crate) fn set_target_mode(&mut self, mode: TargetMode) {
        self.target_mode = mode;
        self.changed();
    }

    pub(crate) fn validation(&self) -> Option<&SaveError> {
        self.validation.as_ref()
    }

    pub(crate) fn set_validation(&mut self, error: SaveError) {
        self.validation = Some(error);
    }

    pub(crate) fn changed(&mut self) {
        self.validation = None;
    }
}
