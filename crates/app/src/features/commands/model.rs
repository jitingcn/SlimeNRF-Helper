use std::fmt;

use serde::{Deserialize, Serialize};

const MAX_CARDS: usize = 128;
const MAX_TITLE_BYTES: usize = 120;
const MAX_DESCRIPTION_BYTES: usize = 1_024;
const MAX_STEPS: usize = 16;
const MAX_COMMAND_BYTES: usize = 64 * 1_024;
const MAX_WIRE_BYTES: usize = 16_383;
const BROADCAST_PREFIX_BYTES: usize = 9;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedCommands {
    next_id: u64,
    cards: Vec<SavedCommandCard>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct SavedCommandId(u64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedCommandCard {
    pub(crate) id: SavedCommandId,
    pub(crate) title: String,
    pub(crate) description: Option<String>,
    pub(crate) icon: Option<CommandCardIcon>,
    pub(crate) pinned: bool,
    pub(crate) target_mode: TargetMode,
    pub(crate) lines: Vec<CommandLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedCommandDraft {
    pub(crate) id: Option<SavedCommandId>,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) icon: Option<CommandCardIcon>,
    pub(crate) pinned: bool,
    pub(crate) target_mode: TargetMode,
    pub(crate) commands_text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TargetMode {
    ConnectedTerminals,
    ReceiverConsoles,
    ReceiverTrackers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandCardIcon {
    Terminal,
    Usb,
    Wireless,
    Settings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct CommandLine(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MoveDirection {
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SaveError {
    pub(crate) field: DraftField,
    pub(crate) line: Option<usize>,
    pub(crate) reason: ValidationReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftField {
    Library,
    Title,
    Description,
    Commands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValidationReason {
    TitleRequired,
    TitleTooLong,
    DescriptionTooLong,
    CommandsRequired,
    BlankLine,
    InvalidLine,
    LineTooLong,
    TooManySteps,
    MacroTooLarge,
    LibraryFull,
    InvalidId,
    DuplicateId,
    InvalidNextId,
    IdExhausted,
    UnknownId,
}

impl Default for SavedCommands {
    fn default() -> Self {
        Self {
            next_id: 1,
            cards: Vec::new(),
        }
    }
}

impl SavedCommandId {
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

impl CommandLine {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl SavedCommandCard {
    pub(crate) fn commands_text(&self) -> String {
        join_editor_lines(&self.lines)
    }
}

impl SavedCommands {
    pub(crate) fn cards(&self) -> &[SavedCommandCard] {
        &self.cards
    }

    pub(crate) fn get(&self, id: SavedCommandId) -> Option<&SavedCommandCard> {
        self.cards.iter().find(|card| card.id == id)
    }

    pub(crate) fn save_draft(
        &mut self,
        draft: SavedCommandDraft,
    ) -> Result<SavedCommandId, SaveError> {
        let index = match draft.id {
            Some(id) => Some(
                self.cards
                    .iter()
                    .position(|card| card.id == id)
                    .ok_or_else(|| error(DraftField::Library, None, ValidationReason::UnknownId))?,
            ),
            None => {
                if self.cards.len() >= MAX_CARDS {
                    return Err(error(
                        DraftField::Library,
                        None,
                        ValidationReason::LibraryFull,
                    ));
                }
                None
            }
        };
        let next_id = if index.is_none() {
            self.next_id
                .checked_add(1)
                .ok_or_else(|| error(DraftField::Library, None, ValidationReason::IdExhausted))?
        } else {
            self.next_id
        };
        let id = draft.id.unwrap_or(SavedCommandId(self.next_id));

        validate_metadata_controls(&draft.title, DraftField::Title)?;
        validate_metadata_controls(&draft.description, DraftField::Description)?;
        let title = trim_metadata(draft.title);
        let description = trim_metadata(draft.description);
        validate_title(&title)?;
        validate_description(&description)?;
        let lines = parse_editor_lines(&draft.commands_text, draft.target_mode)?;
        let description = if description.is_empty() {
            None
        } else {
            Some(description)
        };
        let card = SavedCommandCard {
            id,
            title,
            description,
            icon: draft.icon,
            pinned: draft.pinned,
            target_mode: draft.target_mode,
            lines,
        };
        if let Some(index) = index {
            self.cards[index] = card;
        } else {
            self.cards.push(card);
            self.next_id = next_id;
        }
        Ok(id)
    }

    pub(crate) fn remove(&mut self, id: SavedCommandId) -> bool {
        let Some(index) = self.cards.iter().position(|card| card.id == id) else {
            return false;
        };
        self.cards.remove(index);
        true
    }

    pub(crate) fn set_pinned(&mut self, id: SavedCommandId, pinned: bool) -> bool {
        let Some(card) = self.cards.iter_mut().find(|card| card.id == id) else {
            return false;
        };
        if card.pinned == pinned {
            return false;
        }
        card.pinned = pinned;
        true
    }

    pub(crate) fn move_card(&mut self, id: SavedCommandId, direction: MoveDirection) -> bool {
        let Some(index) = self.cards.iter().position(|card| card.id == id) else {
            return false;
        };
        let destination = match direction {
            MoveDirection::Up => index.checked_sub(1),
            MoveDirection::Down => index.checked_add(1).filter(|next| *next < self.cards.len()),
        };
        let Some(destination) = destination else {
            return false;
        };
        self.cards.swap(index, destination);
        true
    }

    pub(crate) fn validate(&self) -> Result<(), SaveError> {
        if self.cards.len() > MAX_CARDS {
            return Err(error(
                DraftField::Library,
                None,
                ValidationReason::LibraryFull,
            ));
        }
        if self.next_id == 0 {
            return Err(error(
                DraftField::Library,
                None,
                ValidationReason::InvalidNextId,
            ));
        }
        for (index, card) in self.cards.iter().enumerate() {
            if card.id.get() == 0 {
                return Err(error(
                    DraftField::Library,
                    None,
                    ValidationReason::InvalidId,
                ));
            }
            if self.cards[..index].iter().any(|other| other.id == card.id) {
                return Err(error(
                    DraftField::Library,
                    None,
                    ValidationReason::DuplicateId,
                ));
            }
            if self.next_id <= card.id.get() {
                return Err(error(
                    DraftField::Library,
                    None,
                    ValidationReason::InvalidNextId,
                ));
            }
            validate_metadata_controls(&card.title, DraftField::Title)?;
            validate_title(&card.title)?;
            if let Some(description) = &card.description {
                validate_metadata_controls(description, DraftField::Description)?;
                validate_description(description)?;
            }
            validate_lines(&card.lines, card.target_mode)?;
        }
        Ok(())
    }
}

pub(crate) fn parse_editor_lines(
    text: &str,
    mode: TargetMode,
) -> Result<Vec<CommandLine>, SaveError> {
    let mut lines = Vec::new();
    let mut remaining = text;
    let mut line_number = 1;
    let mut first_blank = None;
    let mut text_bytes = 0;
    loop {
        let (line, rest) = match remaining.find(['\r', '\n']) {
            Some(index) => {
                let delimiter_bytes = if remaining[index..].starts_with("\r\n") {
                    2
                } else {
                    1
                };
                (
                    &remaining[..index],
                    Some(&remaining[index + delimiter_bytes..]),
                )
            }
            None => (remaining, None),
        };
        match validate_line(line, mode, line_number) {
            Err(problem) if problem.reason == ValidationReason::BlankLine => {
                first_blank.get_or_insert(line_number);
            }
            Err(problem) => return Err(problem),
            Ok(()) => {
                if let Some(line) = first_blank {
                    return Err(error(
                        DraftField::Commands,
                        Some(line),
                        ValidationReason::BlankLine,
                    ));
                }
                text_bytes += line.len() + usize::from(!lines.is_empty());
                validate_command_budget(lines.len() + 1, text_bytes)?;
                lines.push(CommandLine(line.to_owned()));
            }
        }
        let Some(rest) = rest else {
            break;
        };
        remaining = rest;
        line_number += 1;
    }
    if lines.is_empty() {
        return Err(error(
            DraftField::Commands,
            None,
            ValidationReason::CommandsRequired,
        ));
    }
    Ok(lines)
}

pub(crate) fn join_editor_lines(lines: &[CommandLine]) -> String {
    let capacity =
        lines.iter().map(|line| line.as_str().len()).sum::<usize>() + lines.len().saturating_sub(1);
    let mut text = String::with_capacity(capacity);
    for (index, line) in lines.iter().enumerate() {
        if index != 0 {
            text.push('\n');
        }
        text.push_str(line.as_str());
    }
    text
}

fn validate_title(title: &str) -> Result<(), SaveError> {
    if title.trim().is_empty() {
        return Err(error(
            DraftField::Title,
            None,
            ValidationReason::TitleRequired,
        ));
    }
    if title.len() > MAX_TITLE_BYTES {
        return Err(error(
            DraftField::Title,
            None,
            ValidationReason::TitleTooLong,
        ));
    }
    Ok(())
}

fn validate_description(description: &str) -> Result<(), SaveError> {
    if description.len() > MAX_DESCRIPTION_BYTES {
        return Err(error(
            DraftField::Description,
            None,
            ValidationReason::DescriptionTooLong,
        ));
    }
    Ok(())
}

fn validate_metadata_controls(text: &str, field: DraftField) -> Result<(), SaveError> {
    if text.chars().any(char::is_control) {
        return Err(error(field, None, ValidationReason::InvalidLine));
    }
    Ok(())
}

fn trim_metadata(mut text: String) -> String {
    text.truncate(text.trim_end().len());
    let leading_bytes = text.len() - text.trim_start().len();
    drop(text.drain(..leading_bytes));
    text
}

fn validate_lines(lines: &[CommandLine], mode: TargetMode) -> Result<(), SaveError> {
    if lines.is_empty() {
        return Err(error(
            DraftField::Commands,
            None,
            ValidationReason::CommandsRequired,
        ));
    }
    validate_command_budget(lines.len(), 0)?;
    let mut text_bytes = lines.len() - 1;
    for (index, line) in lines.iter().enumerate() {
        validate_line(line.as_str(), mode, index + 1)?;
        text_bytes += line.as_str().len();
        validate_command_budget(lines.len(), text_bytes)?;
    }
    Ok(())
}

fn validate_line(line: &str, mode: TargetMode, number: usize) -> Result<(), SaveError> {
    let reason = if line
        .chars()
        .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
    {
        Some(ValidationReason::InvalidLine)
    } else if line.trim().is_empty() {
        Some(ValidationReason::BlankLine)
    } else if mode == TargetMode::ReceiverTrackers
        && line
            .split_whitespace()
            .next()
            .is_some_and(|verb| verb.eq_ignore_ascii_case("send"))
    {
        Some(ValidationReason::InvalidLine)
    } else if line.len()
        > MAX_WIRE_BYTES
            - if mode == TargetMode::ReceiverTrackers {
                BROADCAST_PREFIX_BYTES
            } else {
                0
            }
    {
        Some(ValidationReason::LineTooLong)
    } else {
        None
    };
    match reason {
        Some(reason) => Err(error(DraftField::Commands, Some(number), reason)),
        None => Ok(()),
    }
}

fn validate_command_budget(count: usize, text_bytes: usize) -> Result<(), SaveError> {
    let reason = if count > MAX_STEPS {
        Some(ValidationReason::TooManySteps)
    } else if text_bytes > MAX_COMMAND_BYTES {
        Some(ValidationReason::MacroTooLarge)
    } else {
        None
    };
    match reason {
        Some(reason) => Err(error(DraftField::Commands, None, reason)),
        None => Ok(()),
    }
}

fn error(field: DraftField, line: Option<usize>, reason: ValidationReason) -> SaveError {
    SaveError {
        field,
        line,
        reason,
    }
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let field = match self.field {
            DraftField::Library => "Saved command library",
            DraftField::Title => "Title",
            DraftField::Description => "Description",
            DraftField::Commands => "Commands",
        };
        write!(f, "{field}")?;
        if let Some(line) = self.line {
            write!(f, " line {line}")?;
        }
        let reason = match self.reason {
            ValidationReason::TitleRequired => "a title is required",
            ValidationReason::TitleTooLong => "the title exceeds 120 UTF-8 bytes",
            ValidationReason::DescriptionTooLong => "the description exceeds 1024 UTF-8 bytes",
            ValidationReason::CommandsRequired => "at least one command is required",
            ValidationReason::BlankLine => "blank command lines are not allowed",
            ValidationReason::InvalidLine => "the text is invalid for this field or target mode",
            ValidationReason::LineTooLong => "the effective payload exceeds 16383 UTF-8 bytes",
            ValidationReason::TooManySteps => "at most 16 command lines are allowed",
            ValidationReason::MacroTooLarge => "command text exceeds 64 KiB including delimiters",
            ValidationReason::LibraryFull => "at most 128 saved cards are allowed",
            ValidationReason::InvalidId => "saved card IDs must be nonzero",
            ValidationReason::DuplicateId => "saved card IDs must be unique",
            ValidationReason::InvalidNextId => "the next ID must exceed every saved card ID",
            ValidationReason::IdExhausted => "no further saved card ID can be allocated",
            ValidationReason::UnknownId => "the saved card no longer exists",
        };
        write!(f, ": {reason}")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommandCardIcon, CommandLine, DraftField, MoveDirection, SaveError, SavedCommandDraft,
        SavedCommandId, SavedCommands, TargetMode, ValidationReason, join_editor_lines,
        parse_editor_lines,
    };
    use serde_json::{Value, json};

    fn draft(text: &str) -> SavedCommandDraft {
        SavedCommandDraft {
            id: None,
            title: "Status".to_owned(),
            description: String::new(),
            icon: None,
            pinned: false,
            target_mode: TargetMode::ConnectedTerminals,
            commands_text: text.to_owned(),
        }
    }

    fn problem(field: DraftField, line: Option<usize>, reason: ValidationReason) -> SaveError {
        SaveError {
            field,
            line,
            reason,
        }
    }

    fn loaded(document: Value) -> SavedCommands {
        serde_json::from_value(document).unwrap()
    }

    fn valid_document() -> Value {
        json!({
            "next_id": 2,
            "cards": [{
                "id": 1,
                "title": "Status",
                "description": null,
                "icon": "terminal",
                "pinned": true,
                "target_mode": "connected_terminals",
                "lines": ["info"]
            }]
        })
    }

    #[test]
    fn empty_library_is_valid_and_allocates_the_first_id() {
        let mut library = SavedCommands::default();
        assert_eq!(library.validate(), Ok(()));
        assert_eq!(
            serde_json::to_value(&library).unwrap(),
            json!({"next_id": 1, "cards": []})
        );
        let id = library.save_draft(draft("info")).unwrap();
        assert_eq!(id.get(), 1);
        assert_eq!(library.cards().len(), 1);
        assert_eq!(library.get(id).unwrap().commands_text(), "info");
        assert_eq!(library.validate(), Ok(()));
    }

    #[test]
    fn saving_trims_metadata_but_preserves_command_spacing() {
        let mut library = SavedCommands::default();
        let mut input = draft("  info  \r\nget  value\rstatus\n \n");
        input.title = "  Read status  ".to_owned();
        input.description = "  Before a test  ".to_owned();
        input.icon = Some(CommandCardIcon::Usb);
        input.pinned = true;
        input.target_mode = TargetMode::ReceiverConsoles;
        let id = library.save_draft(input).unwrap();
        let card = library.get(id).unwrap();
        assert_eq!(card.title, "Read status");
        assert_eq!(card.description.as_deref(), Some("Before a test"));
        assert_eq!(card.icon, Some(CommandCardIcon::Usb));
        assert!(card.pinned);
        assert_eq!(card.target_mode, TargetMode::ReceiverConsoles);
        assert_eq!(card.commands_text(), "  info  \nget  value\nstatus");
        assert_eq!(card.lines[0].as_str(), "  info  ");
        assert_eq!(library.validate(), Ok(()));
    }

    #[test]
    fn blank_description_becomes_none() {
        let mut library = SavedCommands::default();
        let mut input = draft("info");
        input.description = "  \u{2003}  ".to_owned();
        let id = library.save_draft(input).unwrap();
        assert_eq!(library.get(id).unwrap().description, None);
    }

    #[test]
    fn edits_pin_changes_moves_and_removal_keep_stable_ids() {
        let mut library = SavedCommands::default();
        let first = library.save_draft(draft("info")).unwrap();
        let second = library.save_draft(draft("status")).unwrap();
        let third = library.save_draft(draft("get")).unwrap();
        let mut edit = draft("get  version");
        edit.id = Some(second);
        edit.title = "Edited".to_owned();
        assert_eq!(library.save_draft(edit), Ok(second));
        assert!(library.set_pinned(second, true));
        assert!(!library.set_pinned(second, true));
        assert!(library.set_pinned(second, false));
        assert_eq!(
            library
                .cards()
                .iter()
                .map(|card| card.id.get())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(library.get(second).unwrap().title, "Edited");
        assert_eq!(library.get(second).unwrap().commands_text(), "get  version");
        assert!(library.move_card(third, MoveDirection::Up));
        assert_eq!(
            library
                .cards()
                .iter()
                .map(|card| card.id.get())
                .collect::<Vec<_>>(),
            [1, 3, 2]
        );
        assert!(library.move_card(first, MoveDirection::Down));
        assert_eq!(
            library
                .cards()
                .iter()
                .map(|card| card.id.get())
                .collect::<Vec<_>>(),
            [3, 1, 2]
        );
        assert!(library.remove(first));
        assert_eq!(library.get(first), None);
        assert_eq!(library.save_draft(draft("new")).unwrap().get(), 4);
        assert_eq!(
            library
                .cards()
                .iter()
                .map(|card| card.id.get())
                .collect::<Vec<_>>(),
            [3, 2, 4]
        );
        assert_eq!(library.validate(), Ok(()));
    }

    #[test]
    fn unknown_ids_and_boundary_moves_do_not_mutate() {
        let mut library = SavedCommands::default();
        let first = library.save_draft(draft("info")).unwrap();
        let second = library.save_draft(draft("status")).unwrap();
        let original = library.clone();
        let missing = SavedCommandId(99);
        assert!(!library.remove(missing));
        assert!(!library.set_pinned(missing, true));
        assert!(!library.move_card(missing, MoveDirection::Up));
        assert!(!library.move_card(first, MoveDirection::Up));
        assert!(!library.move_card(second, MoveDirection::Down));
        let mut input = draft("info");
        input.id = Some(missing);
        assert_eq!(
            library.save_draft(input),
            Err(problem(
                DraftField::Library,
                None,
                ValidationReason::UnknownId
            ))
        );
        assert_eq!(library, original);
    }

    #[test]
    fn invalid_new_and_edited_drafts_leave_every_field_and_next_id_unchanged() {
        let mut library = SavedCommands::default();
        let id = library.save_draft(draft("info")).unwrap();
        let original = library.clone();
        for edit_id in [None, Some(id)] {
            let mut input = draft("status\n \nreboot");
            input.id = edit_id;
            input.title = "Changed".to_owned();
            input.pinned = true;
            input.icon = Some(CommandCardIcon::Wireless);
            assert_eq!(
                library.save_draft(input),
                Err(problem(
                    DraftField::Commands,
                    Some(2),
                    ValidationReason::BlankLine
                ))
            );
            assert_eq!(library, original);
        }
        assert_eq!(library.save_draft(draft("status")).unwrap().get(), 2);
    }

    #[test]
    fn the_card_limit_allows_edits_but_rejects_new_cards_atomically() {
        let mut library = SavedCommands::default();
        for _ in 0..128 {
            library.save_draft(draft("info")).unwrap();
        }
        let original = library.clone();
        assert_eq!(
            library.save_draft(draft("status")),
            Err(problem(
                DraftField::Library,
                None,
                ValidationReason::LibraryFull
            ))
        );
        assert_eq!(library, original);
        let mut input = draft("status");
        input.id = Some(SavedCommandId(128));
        assert_eq!(library.save_draft(input), Ok(SavedCommandId(128)));
        assert_eq!(library.cards().len(), 128);
        assert_eq!(
            library.get(SavedCommandId(128)).unwrap().commands_text(),
            "status"
        );
        assert_eq!(library.validate(), Ok(()));
    }

    #[test]
    fn id_exhaustion_rejects_allocation_without_blocking_an_existing_edit() {
        let mut document = valid_document();
        document["next_id"] = json!(u64::MAX);
        let mut library = loaded(document);
        assert_eq!(library.validate(), Ok(()));
        let original = library.clone();
        assert_eq!(
            library.save_draft(draft("status")),
            Err(problem(
                DraftField::Library,
                None,
                ValidationReason::IdExhausted
            ))
        );
        assert_eq!(library, original);
        let mut input = draft("status");
        input.id = Some(SavedCommandId(1));
        assert_eq!(library.save_draft(input), Ok(SavedCommandId(1)));
        assert_eq!(
            library.get(SavedCommandId(1)).unwrap().commands_text(),
            "status"
        );
        assert_eq!(library.validate(), Ok(()));
    }

    #[test]
    fn title_and_description_limits_count_utf8_bytes_after_editor_trimming() {
        let mut library = SavedCommands::default();
        let mut input = draft("info");
        input.title = format!("  {}  ", "é".repeat(60));
        input.description = format!("  {}  ", "é".repeat(512));
        let id = library.save_draft(input).unwrap();
        let card = library.get(id).unwrap();
        assert_eq!(card.title.len(), 120);
        assert_eq!(card.description.as_ref().unwrap().len(), 1024);
        for (title, description, expected) in [
            (
                "é".repeat(61),
                String::new(),
                problem(DraftField::Title, None, ValidationReason::TitleTooLong),
            ),
            (
                "Status".to_owned(),
                "é".repeat(513),
                problem(
                    DraftField::Description,
                    None,
                    ValidationReason::DescriptionTooLong,
                ),
            ),
            (
                " \u{2003} ".to_owned(),
                String::new(),
                problem(DraftField::Title, None, ValidationReason::TitleRequired),
            ),
        ] {
            let original = library.clone();
            let mut input = draft("info");
            input.title = title;
            input.description = description;
            assert_eq!(library.save_draft(input), Err(expected));
            assert_eq!(library, original);
        }
    }

    #[test]
    fn metadata_controls_are_not_hidden_by_trimming() {
        let mut library = SavedCommands::default();
        for (title, description, field) in [
            ("\nStatus", "", DraftField::Title),
            ("Status\t", "", DraftField::Title),
            ("Status", "\rNotes", DraftField::Description),
            ("Status", "Notes\0", DraftField::Description),
        ] {
            let mut input = draft("info");
            input.title = title.to_owned();
            input.description = description.to_owned();
            assert_eq!(
                library.save_draft(input),
                Err(problem(field, None, ValidationReason::InvalidLine))
            );
        }
        assert_eq!(library, SavedCommands::default());
    }

    #[test]
    fn editor_framing_accepts_lf_crlf_and_lone_cr_without_changing_spaces() {
        let lines = parse_editor_lines(
            "  first  \r\nsecond\rthird\nfourth \r\n\r \n",
            TargetMode::ConnectedTerminals,
        )
        .unwrap();
        assert_eq!(
            lines.iter().map(CommandLine::as_str).collect::<Vec<_>>(),
            ["  first  ", "second", "third", "fourth "]
        );
        assert_eq!(
            join_editor_lines(&lines),
            "  first  \nsecond\nthird\nfourth "
        );
        assert_eq!(join_editor_lines(&[]), "");
    }

    #[test]
    fn empty_and_whitespace_only_editor_input_has_no_commands() {
        for text in ["", " ", "\r\n\r", " \n\u{2003}\r\n"] {
            assert_eq!(
                parse_editor_lines(text, TargetMode::ReceiverConsoles),
                Err(problem(
                    DraftField::Commands,
                    None,
                    ValidationReason::CommandsRequired
                ))
            );
        }
    }

    #[test]
    fn interior_blank_lines_report_the_logical_one_based_line() {
        for text in ["info\n \nstatus", "info\r\n \r\nstatus", "info\r \rstatus"] {
            assert_eq!(
                parse_editor_lines(text, TargetMode::ConnectedTerminals),
                Err(problem(
                    DraftField::Commands,
                    Some(2),
                    ValidationReason::BlankLine
                ))
            );
        }
        assert_eq!(
            parse_editor_lines("\r\ninfo", TargetMode::ConnectedTerminals),
            Err(problem(
                DraftField::Commands,
                Some(1),
                ValidationReason::BlankLine
            ))
        );
    }

    #[test]
    fn command_controls_and_unicode_separators_are_rejected() {
        for character in [
            '\0', '\t', '\u{000b}', '\u{007f}', '\u{0085}', '\u{2028}', '\u{2029}',
        ] {
            let text = format!("info\r\nget{character}value");
            assert_eq!(
                parse_editor_lines(&text, TargetMode::ConnectedTerminals),
                Err(problem(
                    DraftField::Commands,
                    Some(2),
                    ValidationReason::InvalidLine
                ))
            );
        }
        assert_eq!(
            parse_editor_lines("info\n\t", TargetMode::ConnectedTerminals),
            Err(problem(
                DraftField::Commands,
                Some(2),
                ValidationReason::InvalidLine
            ))
        );
    }

    #[test]
    fn only_tracker_broadcast_rejects_a_leading_send_token() {
        for text in [
            "send all info",
            " SEND 1 info ",
            "send",
            "send all send 1 info",
        ] {
            assert_eq!(
                parse_editor_lines(text, TargetMode::ReceiverTrackers),
                Err(problem(
                    DraftField::Commands,
                    Some(1),
                    ValidationReason::InvalidLine
                ))
            );
            for mode in [TargetMode::ConnectedTerminals, TargetMode::ReceiverConsoles] {
                assert_eq!(
                    join_editor_lines(&parse_editor_lines(text, mode).unwrap()),
                    text
                );
            }
        }
        assert_eq!(
            join_editor_lines(
                &parse_editor_lines("sender info", TargetMode::ReceiverTrackers).unwrap()
            ),
            "sender info"
        );
    }

    #[test]
    fn effective_wire_limits_count_utf8_and_the_nine_byte_broadcast_prefix() {
        for mode in [TargetMode::ConnectedTerminals, TargetMode::ReceiverConsoles] {
            let at_limit = format!("{}a", "é".repeat(8191));
            assert_eq!(
                parse_editor_lines(&at_limit, mode).unwrap()[0]
                    .as_str()
                    .len(),
                16383
            );
            assert_eq!(
                parse_editor_lines(&format!("{at_limit}a"), mode),
                Err(problem(
                    DraftField::Commands,
                    Some(1),
                    ValidationReason::LineTooLong
                ))
            );
        }
        let broadcast_limit = "x".repeat(16374);
        assert_eq!(
            parse_editor_lines(&broadcast_limit, TargetMode::ReceiverTrackers).unwrap()[0]
                .as_str()
                .len(),
            16374
        );
        assert_eq!(
            parse_editor_lines(&format!("{broadcast_limit}x"), TargetMode::ReceiverTrackers),
            Err(problem(
                DraftField::Commands,
                Some(1),
                ValidationReason::LineTooLong
            ))
        );
    }

    #[test]
    fn step_count_excludes_trailing_blank_editor_lines() {
        let sixteen = ["info"; 16].join("\n");
        assert_eq!(
            parse_editor_lines(&format!("{sixteen}\r\n \r"), TargetMode::ConnectedTerminals)
                .unwrap()
                .len(),
            16
        );
        assert_eq!(
            parse_editor_lines(
                &format!("{sixteen}\nstatus"),
                TargetMode::ConnectedTerminals
            ),
            Err(problem(
                DraftField::Commands,
                None,
                ValidationReason::TooManySteps
            ))
        );
    }

    #[test]
    fn macro_byte_limit_counts_canonical_delimiters_but_not_final_enter() {
        let text = [
            "x".repeat(16383),
            "x".repeat(16383),
            "x".repeat(16383),
            "x".repeat(16382),
            "x".to_owned(),
        ]
        .join("\n");
        assert_eq!(text.len(), 65536);
        let crlf = text.replace('\n', "\r\n");
        let lines =
            parse_editor_lines(&format!("{crlf}\r\n"), TargetMode::ConnectedTerminals).unwrap();
        assert_eq!(join_editor_lines(&lines), text);
        assert_eq!(
            parse_editor_lines(&format!("{text}x"), TargetMode::ConnectedTerminals),
            Err(problem(
                DraftField::Commands,
                None,
                ValidationReason::MacroTooLarge
            ))
        );
        let mut library = SavedCommands::default();
        let id = library.save_draft(draft(&text)).unwrap();
        assert_eq!(library.get(id).unwrap().commands_text().len(), 65536);
        assert_eq!(library.validate(), Ok(()));
    }

    #[test]
    fn serialization_uses_transparent_values_and_snake_case_variants() {
        let library = loaded(valid_document());
        assert_eq!(library.validate(), Ok(()));
        assert_eq!(serde_json::to_value(&library).unwrap(), valid_document());
        for (mode, name) in [
            (TargetMode::ConnectedTerminals, "connected_terminals"),
            (TargetMode::ReceiverConsoles, "receiver_consoles"),
            (TargetMode::ReceiverTrackers, "receiver_trackers"),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), json!(name));
        }
        for (icon, name) in [
            (CommandCardIcon::Terminal, "terminal"),
            (CommandCardIcon::Usb, "usb"),
            (CommandCardIcon::Wireless, "wireless"),
            (CommandCardIcon::Settings, "settings"),
        ] {
            assert_eq!(serde_json::to_value(icon).unwrap(), json!(name));
        }
        assert_eq!(
            serde_json::from_value::<SavedCommands>(serde_json::to_value(&library).unwrap())
                .unwrap(),
            library
        );
    }

    #[test]
    fn loaded_ids_must_be_nonzero_unique_and_below_next_id() {
        let mut zero = valid_document();
        zero["cards"][0]["id"] = json!(0);
        assert_eq!(
            loaded(zero).validate(),
            Err(problem(
                DraftField::Library,
                None,
                ValidationReason::InvalidId
            ))
        );
        let mut duplicate = valid_document();
        let card = duplicate["cards"][0].clone();
        duplicate["cards"].as_array_mut().unwrap().push(card);
        assert_eq!(
            loaded(duplicate).validate(),
            Err(problem(
                DraftField::Library,
                None,
                ValidationReason::DuplicateId
            ))
        );
        for next_id in [0, 1] {
            let mut stale = valid_document();
            stale["next_id"] = json!(next_id);
            assert_eq!(
                loaded(stale).validate(),
                Err(problem(
                    DraftField::Library,
                    None,
                    ValidationReason::InvalidNextId
                ))
            );
        }
        assert_eq!(
            loaded(json!({"next_id": 0, "cards": []})).validate(),
            Err(problem(
                DraftField::Library,
                None,
                ValidationReason::InvalidNextId
            ))
        );
    }

    #[test]
    fn loaded_card_count_and_metadata_are_validated_without_normalization() {
        let mut excessive = valid_document();
        let card = excessive["cards"][0].clone();
        excessive["cards"] = Value::Array(vec![card; 129]);
        assert_eq!(
            loaded(excessive).validate(),
            Err(problem(
                DraftField::Library,
                None,
                ValidationReason::LibraryFull
            ))
        );
        for (field, value, expected) in [
            (
                "title",
                "  ".to_owned(),
                problem(DraftField::Title, None, ValidationReason::TitleRequired),
            ),
            (
                "title",
                "x".repeat(121),
                problem(DraftField::Title, None, ValidationReason::TitleTooLong),
            ),
            (
                "title",
                "Status\n".to_owned(),
                problem(DraftField::Title, None, ValidationReason::InvalidLine),
            ),
            (
                "description",
                "x".repeat(1025),
                problem(
                    DraftField::Description,
                    None,
                    ValidationReason::DescriptionTooLong,
                ),
            ),
            (
                "description",
                "Notes\t".to_owned(),
                problem(DraftField::Description, None, ValidationReason::InvalidLine),
            ),
        ] {
            let mut document = valid_document();
            document["cards"][0][field] = json!(value);
            assert_eq!(loaded(document).validate(), Err(expected));
        }
    }

    #[test]
    fn loaded_command_lines_validate_independently_of_editor_framing() {
        for (lines, expected) in [
            (
                json!([]),
                problem(
                    DraftField::Commands,
                    None,
                    ValidationReason::CommandsRequired,
                ),
            ),
            (
                json!([" "]),
                problem(DraftField::Commands, Some(1), ValidationReason::BlankLine),
            ),
            (
                json!(["info\nstatus"]),
                problem(DraftField::Commands, Some(1), ValidationReason::InvalidLine),
            ),
            (
                json!(["info\r"]),
                problem(DraftField::Commands, Some(1), ValidationReason::InvalidLine),
            ),
            (
                json!(["info", "get\u{2028}value"]),
                problem(DraftField::Commands, Some(2), ValidationReason::InvalidLine),
            ),
            (
                json!(["x".repeat(16384)]),
                problem(DraftField::Commands, Some(1), ValidationReason::LineTooLong),
            ),
            (
                json!(vec!["info"; 17]),
                problem(DraftField::Commands, None, ValidationReason::TooManySteps),
            ),
            (
                json!(vec!["x".repeat(16383); 5]),
                problem(DraftField::Commands, None, ValidationReason::MacroTooLarge),
            ),
        ] {
            let mut document = valid_document();
            document["cards"][0]["lines"] = lines;
            assert_eq!(loaded(document).validate(), Err(expected));
        }
    }

    #[test]
    fn loaded_tracker_commands_reject_nested_send_and_oversized_effective_payloads() {
        for (line, reason) in [
            (" SEND all info ".to_owned(), ValidationReason::InvalidLine),
            ("x".repeat(16375), ValidationReason::LineTooLong),
        ] {
            let mut document = valid_document();
            document["cards"][0]["target_mode"] = json!("receiver_trackers");
            document["cards"][0]["lines"] = json!([line]);
            assert_eq!(
                loaded(document).validate(),
                Err(problem(DraftField::Commands, Some(1), reason))
            );
        }
    }

    #[test]
    fn validation_display_includes_the_field_and_one_based_line() {
        assert_eq!(
            problem(DraftField::Commands, Some(2), ValidationReason::BlankLine).to_string(),
            "Commands line 2: blank command lines are not allowed"
        );
        assert_eq!(
            problem(DraftField::Library, None, ValidationReason::UnknownId).to_string(),
            "Saved command library: the saved card no longer exists"
        );
    }
}
