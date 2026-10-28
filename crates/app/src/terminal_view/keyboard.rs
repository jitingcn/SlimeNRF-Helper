use std::{cell::RefCell, rc::Rc};

use gpui_kit::base::TextSelection;
use gpui_kit::{App, KeyDownEvent, Modifiers, Pixels, Point, ScrollHandle, Window, point, px};

use super::{
    TranscriptState,
    geometry::{RowAffinity, ViewportAnchor},
};

#[derive(Clone, Copy)]
enum Navigation {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    DocumentStart,
    DocumentEnd,
    PageUp,
    PageDown,
    SelectAll,
}

impl Navigation {
    fn from_event(event: &KeyDownEvent, cx: &App) -> Option<Self> {
        let modifiers = event.keystroke.modifiers;
        let secondary = modifiers.secondary(cx);
        let shortcut = if secondary {
            Modifiers::secondary_key(cx)
        } else {
            Modifiers::none()
        };
        if modifiers.alt
            || modifiers.function
            || modifiers.control != shortcut.control
            || modifiers.platform != shortcut.platform
        {
            return None;
        }
        match (event.keystroke.key.as_str(), secondary, modifiers.shift) {
            ("left", false, _) => Some(Self::Left),
            ("right", false, _) => Some(Self::Right),
            ("up", false, _) => Some(Self::Up),
            ("down", false, _) => Some(Self::Down),
            ("home", false, _) => Some(Self::Home),
            ("end", false, _) => Some(Self::End),
            ("home", true, _) => Some(Self::DocumentStart),
            ("end", true, _) => Some(Self::DocumentEnd),
            ("pageup", false, false) => Some(Self::PageUp),
            ("pagedown", false, false) => Some(Self::PageDown),
            ("a", true, false) => Some(Self::SelectAll),
            _ => None,
        }
    }
}

/// Handles navigation only on the focused transcript's key-dispatch path.
///
/// The caller pauses Follow, stops propagation and notifies after `true`.
/// Copy remains the owner's existing Action; unrelated shortcuts are untouched.
pub(crate) fn handle_key_down(
    state: &Rc<RefCell<TranscriptState>>,
    event: &KeyDownEvent,
    scroll: &ScrollHandle,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let Some(navigation) = Navigation::from_event(event, cx) else {
        return false;
    };
    // Clearing Root's geometric projection synchronously notifies participants.
    // Do this before borrowing the retained byte selection to avoid re-entry.
    TextSelection::clear(window, cx);
    let mut state = state.borrow_mut();
    if state.lines.is_empty() || state.style.is_none() {
        return true;
    }
    let byte = state.keyboard_cursor(scroll);
    let index = state.line_index_for_byte(byte);
    let mut changed = state.measure_line(index, false, window, cx);
    let neighbor = match navigation {
        Navigation::Up | Navigation::Left => index.checked_sub(1),
        Navigation::Down | Navigation::Right => {
            (index + 1 < state.lines.len()).then_some(index + 1)
        }
        Navigation::DocumentStart => Some(0),
        Navigation::DocumentEnd | Navigation::SelectAll => Some(state.lines.len() - 1),
        _ => None,
    };
    if let Some(index) = neighbor {
        changed |= state.measure_line(index, false, window, cx);
    }
    if changed {
        state.rebuild_prefix();
        state.extent_dirty = true;
    }
    state.navigate(navigation, event.keystroke.modifiers.shift, scroll);
    true
}

impl TranscriptState {
    fn line_index_for_byte(&self, byte: usize) -> usize {
        self.lines
            .iter()
            .rposition(|line| line.offset <= byte)
            .unwrap_or(0)
    }

    fn keyboard_cursor(&self, scroll: &ScrollHandle) -> usize {
        self.endpoints[1].unwrap_or_else(|| {
            let top = self.presented_top(scroll.offset()).unwrap_or(px(0.));
            self.capture_anchor(top)
                .and_then(|anchor| {
                    self.anchor_index(anchor)
                        .map(|index| self.lines[index].offset + anchor.byte)
                })
                .unwrap_or(0)
        })
    }

    pub(super) fn caret_position(
        &self,
        byte: usize,
        affinity: RowAffinity,
    ) -> Option<Point<Pixels>> {
        let index = self.line_index_for_byte(byte);
        let line = self.lines.get(index)?;
        let geometry = line
            .geometry
            .as_ref()
            .filter(|geometry| geometry.key == self.layout_key)?;
        let byte = (byte - line.offset).min(geometry.content_layout.len());
        Some(geometry.position(byte, affinity) + point(px(0.), self.line_top(index)))
    }

    fn adjacent_byte(&self, byte: usize, forward: bool) -> usize {
        let index = self.line_index_for_byte(byte);
        let line = &self.lines[index];
        let local = byte - line.offset;
        if forward {
            line.text[local..]
                .chars()
                .next()
                .map_or(byte, |character| byte + character.len_utf8())
        } else if local > 0 {
            line.offset + line.text[..local].char_indices().next_back().unwrap().0
        } else if let Some(previous) = index.checked_sub(1).map(|index| &self.lines[index]) {
            previous.offset
                + previous
                    .text
                    .char_indices()
                    .next_back()
                    .map_or(0, |(byte, _)| byte)
        } else {
            0
        }
    }

    fn visual_destination(&mut self, byte: usize, navigation: Navigation) -> (usize, RowAffinity) {
        let index = self.line_index_for_byte(byte);
        let line = &self.lines[index];
        let geometry = line.geometry.as_ref().unwrap();
        let local = (byte - line.offset).min(geometry.content_layout.len());
        let row = geometry.row_for_byte(local, self.caret_affinity);
        match navigation {
            Navigation::Home => (
                line.offset + geometry.rows[row].bytes.start,
                RowAffinity::Downstream,
            ),
            Navigation::End => (
                line.offset + geometry.rows[row].bytes.end,
                RowAffinity::Upstream,
            ),
            Navigation::Up | Navigation::Down => {
                let x = *self
                    .preferred_x
                    .get_or_insert_with(|| geometry.position(local, self.caret_affinity).x);
                let (index, row) = match navigation {
                    Navigation::Up if row > 0 => (index, row - 1),
                    Navigation::Up if index > 0 => {
                        let previous = index - 1;
                        (
                            previous,
                            self.lines[previous].geometry.as_ref().unwrap().rows.len() - 1,
                        )
                    }
                    Navigation::Down if row + 1 < geometry.rows.len() => (index, row + 1),
                    Navigation::Down if index + 1 < self.lines.len() => (index + 1, 0),
                    _ => return (byte, self.caret_affinity),
                };
                let line = &self.lines[index];
                let geometry = line.geometry.as_ref().unwrap();
                let bounds = &geometry.rows[row].bytes;
                // Vertical movement lands in content, never in the newline's
                // pointer-selection band to the right of a short visual row.
                let local = geometry
                    .byte_at_point(&line.text, point(x, geometry.key.row_height * row as f32))
                    .min(bounds.end);
                (
                    line.offset + local,
                    if local == bounds.end {
                        RowAffinity::Upstream
                    } else {
                        RowAffinity::Downstream
                    },
                )
            }
            _ => unreachable!("only visual row navigation uses shaped destinations"),
        }
    }

    fn set_keyboard_top(&mut self, top: Pixels, scroll: &ScrollHandle) {
        let Some(frame) = self.presented.as_ref() else {
            return;
        };
        let maximum = (self.height() - frame.viewport.size.height).max(px(0.));
        let top = top.clamp(px(0.), maximum);
        let current = self.presented_top(scroll.offset()).unwrap_or(px(0.));
        let offset = scroll.offset() + point(px(0.), current - top);
        scroll.set_offset(offset);
        // A correction queued for an older viewport must not undo keyboard input.
        self.pending = None;
    }

    fn reveal_keyboard_cursor(&mut self, byte: usize, scroll: &ScrollHandle) {
        let Some(position) = self.caret_position(byte, self.caret_affinity) else {
            return;
        };
        let Some(frame) = self.presented.as_ref() else {
            return;
        };
        let viewport = frame.viewport.size;
        let top = self.presented_top(scroll.offset()).unwrap_or(px(0.));
        let target = if position.y < top {
            position.y
        } else if position.y + self.layout_key.row_height > top + viewport.height {
            position.y + self.layout_key.row_height - viewport.height
        } else {
            top
        };
        let left = frame.viewport.left() - frame.origin.x - (scroll.offset().x - frame.offset.x);
        let horizontal = if position.x < left {
            left - position.x
        } else if position.x >= left + viewport.width {
            left + viewport.width - position.x - px(1.)
        } else {
            px(0.)
        };
        self.set_keyboard_top(target, scroll);
        if self.layout_key.wrap_width.is_none() && horizontal != px(0.) {
            let mut offset = scroll.offset();
            offset.x = (offset.x + horizontal).min(px(0.));
            scroll.set_offset(offset);
        }
        // Retain a document anchor at the caret's screen row, rather than a
        // provisional prefix height. Incremental reflow before an offscreen
        // destination must not move that destination out of the viewport.
        let top = self.presented_top(scroll.offset()).unwrap_or(px(0.));
        let index = self.line_index_for_byte(byte);
        let line = &self.lines[index];
        self.anchor = Some(ViewportAnchor {
            generation: self.identity.unwrap().0,
            line_id: line.id,
            byte: (byte - line.offset).min(line.geometry.as_ref().unwrap().content_layout.len()),
            affinity: self.caret_affinity,
            intra_row_y: top - position.y,
        });
        self.presented.as_mut().unwrap().offset.y = scroll.offset().y;
    }

    fn navigate(&mut self, navigation: Navigation, extend: bool, scroll: &ScrollHandle) {
        if matches!(navigation, Navigation::PageUp | Navigation::PageDown) {
            if let Some(frame) = &self.presented {
                let step = (frame.viewport.size.height - self.layout_key.row_height)
                    .max(self.layout_key.row_height);
                let top = self.presented_top(scroll.offset()).unwrap_or(px(0.));
                self.set_keyboard_top(
                    top + if matches!(navigation, Navigation::PageUp) {
                        -step
                    } else {
                        step
                    },
                    scroll,
                );
            }
            return;
        }
        if matches!(navigation, Navigation::SelectAll) {
            self.endpoints = [Some(0), Some(self.byte_len)];
            self.selected = (self.byte_len != 0).then_some(0..self.byte_len);
            self.completed = true;
            self.keyboard_caret = true;
            self.preferred_x = None;
            self.caret_affinity = RowAffinity::Downstream;
            return;
        }
        let cursor = self.keyboard_cursor(scroll);
        let collapse = (!extend)
            .then_some(self.selected.as_ref())
            .flatten()
            .and_then(|range| match navigation {
                Navigation::Left => Some(range.start),
                Navigation::Right => Some(range.end),
                _ => None,
            });
        let (byte, affinity) = if let Some(byte) = collapse {
            (byte, RowAffinity::Downstream)
        } else {
            match navigation {
                Navigation::Left => (self.adjacent_byte(cursor, false), RowAffinity::Upstream),
                Navigation::Right => (self.adjacent_byte(cursor, true), RowAffinity::Downstream),
                Navigation::DocumentStart => (0, RowAffinity::Downstream),
                Navigation::DocumentEnd => (self.byte_len, RowAffinity::Downstream),
                _ => self.visual_destination(cursor, navigation),
            }
        };
        let anchor = if extend {
            self.endpoints[0].unwrap_or(cursor)
        } else {
            byte
        };
        self.endpoints = [Some(anchor), Some(byte)];
        self.selected = (anchor != byte).then_some(anchor.min(byte)..anchor.max(byte));
        self.completed = true;
        self.keyboard_caret = true;
        self.caret_affinity = affinity;
        if !matches!(navigation, Navigation::Up | Navigation::Down) {
            self.preferred_x = None;
        }
        self.reveal_keyboard_cursor(byte, scroll);
    }
}

#[cfg(test)]
mod tests {
    use super::Navigation;
    use crate::terminal_view::{PresentedFrame, TranscriptState, geometry::geometry_fixture};
    use gpui_kit::{Bounds, ScrollHandle, point, px, size};
    use slimenrf_terminal_core::{MAX_LINES, TerminalBuffer};

    fn fixture(text: &str, seams: &[&[usize]]) -> (TerminalBuffer, TranscriptState, ScrollHandle) {
        let mut buffer = TerminalBuffer::new();
        buffer.push(text.as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        for (index, line) in state.lines.iter_mut().enumerate() {
            let mut geometry =
                geometry_fixture(&line.text, seams.get(index).copied().unwrap_or(&[]));
            geometry.key = state.layout_key;
            line.row_count = geometry.rows.len() as u32;
            line.geometry = Some(geometry);
        }
        state.rebuild_prefix();
        state.anchor = state.capture_anchor(px(0.));
        state.presented = Some(PresentedFrame {
            key: state.layout_key,
            identity: state.identity,
            projection_generation: state.projection_generation,
            revision: state.revision,
            origin: point(px(0.), px(0.)),
            viewport: Bounds::new(point(px(0.), px(0.)), size(px(100.), px(40.))),
            visible: 0..1,
            offset: point(px(0.), px(0.)),
            follow: false,
            height: state.height(),
        });
        (buffer, state, ScrollHandle::default())
    }

    #[test]
    fn keyboard_selection_keeps_unicode_bytes_real_newlines_and_direction() {
        let (_, mut state, scroll) = fixture("a界世界 🦀\n\nend", &[&[4, 11]]);
        state.navigate(Navigation::Home, false, &scroll);
        state.navigate(Navigation::Right, false, &scroll);
        for expected in [
            "界",
            "界世",
            "界世界",
            "界世界 ",
            "界世界 🦀",
            "界世界 🦀\n",
        ] {
            state.navigate(Navigation::Right, true, &scroll);
            assert_eq!(state.selected_text().as_deref(), Some(expected));
        }
        state.navigate(Navigation::Left, false, &scroll);
        assert_eq!(state.endpoints, [Some(1), Some(1)]);
        assert_eq!(state.selected_text(), None);
        state.navigate(Navigation::DocumentEnd, true, &scroll);
        assert_eq!(state.selected_text().as_deref(), Some("界世界 🦀\n\nend"));
        state.navigate(Navigation::DocumentStart, true, &scroll);
        assert_eq!(state.selected_text().as_deref(), Some("a"));
    }

    #[test]
    fn visual_rows_retain_preferred_column_and_home_end_affinity() {
        let (_, mut state, scroll) = fixture("a界世界 🦀\nz\n012345", &[&[4, 11]]);
        state.navigate(Navigation::End, true, &scroll);
        assert_eq!(state.selected_text().as_deref(), Some("a界"));
        state.navigate(Navigation::Down, true, &scroll);
        assert_eq!(state.endpoints[1], Some(10));
        state.navigate(Navigation::Down, true, &scroll);
        assert_eq!(state.endpoints[1], Some(15));
        state.navigate(Navigation::Down, true, &scroll);
        assert_eq!(state.endpoints[1], Some(17));
        state.navigate(Navigation::Down, true, &scroll);
        assert_eq!(state.endpoints[1], Some(20));
        state.navigate(Navigation::Up, true, &scroll);
        assert_eq!(state.endpoints[1], Some(17));
        state.navigate(Navigation::Up, true, &scroll);
        state.navigate(Navigation::Home, false, &scroll);
        assert_eq!(state.endpoints[1], Some(11));
        assert_eq!(state.selected_text(), None);
    }

    #[test]
    fn pages_only_scroll_and_caret_navigation_reveals_without_follow() {
        let (_, mut state, scroll) = fixture("one\ntwo\nthree\nfour\nfive\nsix", &[]);
        state.navigate(Navigation::PageDown, false, &scroll);
        assert_eq!(scroll.offset().y, px(-20.));
        assert_eq!(state.endpoints, [None, None]);
        state.navigate(Navigation::Home, false, &scroll);
        assert_eq!(state.endpoints[1], Some(4));
        state.navigate(Navigation::DocumentEnd, true, &scroll);
        assert_eq!(scroll.offset().y, px(-80.));
        let selected = state.selected_text();
        state.navigate(Navigation::PageUp, false, &scroll);
        assert_eq!(scroll.offset().y, px(-60.));
        assert_eq!(state.selected_text(), selected);
        state.navigate(Navigation::SelectAll, false, &scroll);
        assert_eq!(scroll.offset().y, px(-60.));
        assert_eq!(
            state.selected_text().as_deref(),
            Some("one\ntwo\nthree\nfour\nfive\nsix")
        );
        state.navigate(Navigation::DocumentStart, false, &scroll);
        assert_eq!(scroll.offset().y, px(0.));
    }

    #[test]
    fn document_end_keeps_caret_visible_when_provisional_prefix_rows_expand() {
        let (_, mut state, scroll) = fixture("abcdefghij\nmiddle\nlast", &[]);
        state.navigate(Navigation::DocumentEnd, false, &scroll);
        assert_eq!(state.presented_top(scroll.offset()), Some(px(20.)));
        let mut geometry = geometry_fixture(&state.lines[0].text, &[2, 4, 6, 8]);
        geometry.key = state.layout_key;
        state.lines[0].row_count = geometry.rows.len() as u32;
        state.lines[0].geometry = Some(geometry);
        state.rebuild_prefix();
        assert_eq!(state.presented_top(scroll.offset()), Some(px(100.)));
        assert_eq!(
            state
                .caret_position(state.byte_len, state.caret_affinity)
                .unwrap()
                .y
                - state.presented_top(scroll.offset()).unwrap(),
            px(20.)
        );
    }

    #[test]
    fn keyboard_selection_survives_reflow_and_append_but_not_trimmed_identity() {
        let (mut buffer, mut state, scroll) = fixture("界🦀\nlast", &[&[3]]);
        state.navigate(Navigation::SelectAll, false, &scroll);
        buffer.push(b" appended");
        state.sync_buffer(&buffer, 1);
        assert_eq!(state.selected_text().as_deref(), Some("界🦀\nlast"));
        state.set_wrap_width(Some(px(30.)));
        assert_eq!(state.selected_text().as_deref(), Some("界🦀\nlast"));
        buffer.push("\nx".repeat(MAX_LINES).as_bytes());
        state.sync_buffer(&buffer, 1);
        assert_eq!(state.selected_text(), None);
        assert_eq!(state.endpoints, [None, None]);
        assert!(!state.keyboard_caret);
    }
}
