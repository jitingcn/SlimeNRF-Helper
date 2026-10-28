mod filter;
mod geometry;
mod keyboard;

#[cfg(test)]
mod lifecycle_tests;

use std::{cell::RefCell, collections::VecDeque, ops::Range, rc::Rc};

use geometry::{LayoutKey, LineGeometry, RowAffinity, ViewportAnchor, line_at_y, visible_lines};
use gpui_kit::base::{
    TextSelectionContentKey, TextSelectionCoverage, TextSelectionHandle, TextSelectionRegistration,
    TextSelectionRun,
};
use gpui_kit::{prelude::*, *};

pub(crate) use filter::LogFilter;
pub(crate) use keyboard::handle_key_down;

use slimenrf_terminal_core::{AnsiColor, AnsiStyle, StyleRun, TerminalBuffer};

const ROW_HEIGHT_REM: f32 = 1.25;
const REFLOW_LINES: usize = 8;
const REFLOW_BYTES: usize = 32 * 1024;

#[derive(Default)]
pub(crate) struct TranscriptState {
    lines: VecDeque<CachedLine>,
    scratch_lines: VecDeque<CachedLine>,
    source_lines: VecDeque<SourceLine>,
    log_filter: LogFilter,
    projected_filter: LogFilter,
    projection_dirty: bool,
    projection_generation: u64,
    content_key_epoch: u32,
    byte_len: usize,
    identity: Option<(u64, u64)>,
    revision: Option<u64>,
    painted: Vec<PaintedLine>,
    runs: Vec<TextSelectionRun>,
    selected: Option<Range<usize>>,
    endpoints: [Option<usize>; 2],
    caret_affinity: RowAffinity,
    preferred_x: Option<Pixels>,
    keyboard_caret: bool,
    completed: bool,
    callbacks_installed: bool,
    style: Option<RenderStyle>,
    extent_dirty: bool,
    width: Pixels,
    layout_key: LayoutKey,
    reflow_generation: u64,
    reflow_cursor: usize,
    prefix_rows: Vec<u32>,
    anchor: Option<ViewportAnchor>,
    presented: Option<PresentedFrame>,
    hit_lines: Vec<usize>,
    pending: Option<FrameCorrection>,
    frame_queued: bool,
    frame_work: ReflowWork,
}

impl TranscriptState {
    fn rebuild_prefix(&mut self) {
        self.prefix_rows.clear();
        self.prefix_rows.push(0);
        let mut rows = 0;
        self.width = px(0.);
        for line in &self.lines {
            rows += line.row_count;
            self.prefix_rows.push(rows);
            self.width = self.width.max(line.width.unwrap_or(px(0.)));
        }
    }

    fn height(&self) -> Pixels {
        self.layout_key.row_height * self.prefix_rows.last().copied().unwrap_or(0) as f32
    }

    fn line_top(&self, index: usize) -> Pixels {
        self.layout_key.row_height * self.prefix_rows[index] as f32
    }

    fn exact(&self, index: usize) -> bool {
        self.lines[index]
            .geometry
            .as_ref()
            .is_some_and(|geometry| geometry.key == self.layout_key)
    }

    fn set_wrap_width(&mut self, width: Option<Pixels>) -> bool {
        if self.layout_key.wrap_width == width {
            return false;
        }
        self.layout_key.wrap_width = width;
        self.preferred_x = None;
        self.reflow_generation = self.reflow_generation.wrapping_add(1);
        self.reflow_cursor = 0;
        true
    }

    fn anchor_index(&self, anchor: ViewportAnchor) -> Option<usize> {
        (self.identity?.0 == anchor.generation).then_some(())?;
        self.lines
            .iter()
            .position(|line| line.id == anchor.line_id)
            .or_else(|| (!self.lines.is_empty()).then_some(0))
    }

    fn anchor_top(&self, anchor: ViewportAnchor) -> Option<Pixels> {
        let index = self.anchor_index(anchor)?;
        let line = &self.lines[index];
        let geometry = line.geometry.as_ref()?;
        let mut byte = if line.id == anchor.line_id {
            anchor.byte.min(geometry.content_layout.len())
        } else {
            0
        };
        while !line.text.is_char_boundary(byte) {
            byte -= 1;
        }
        Some(self.line_top(index) + geometry.position(byte, anchor.affinity).y + anchor.intra_row_y)
    }

    fn presented_top(&self, offset: Point<Pixels>) -> Option<Pixels> {
        self.presented.as_ref().map(|frame| {
            self.anchor
                .and_then(|anchor| self.anchor_top(anchor))
                .unwrap_or(frame.viewport.top() - frame.origin.y)
                - (offset.y - frame.offset.y)
        })
    }

    fn viewport_top(
        &self,
        anchor: Option<ViewportAnchor>,
        fallback: Pixels,
        viewport_height: Pixels,
        follow: bool,
        reflow: bool,
    ) -> Pixels {
        let maximum = (self.height() - viewport_height).max(px(0.));
        if follow && reflow {
            maximum
        } else {
            anchor
                .and_then(|anchor| self.anchor_top(anchor))
                .unwrap_or(fallback)
                .clamp(px(0.), maximum)
        }
    }

    fn viewport_anchor(
        &self,
        prior: Option<ViewportAnchor>,
        top: Pixels,
        follow: bool,
    ) -> Option<ViewportAnchor> {
        prior
            .filter(|anchor| {
                !follow
                    && self.anchor_top(*anchor) == Some(top)
                    && self
                        .anchor_index(*anchor)
                        .is_some_and(|index| self.lines[index].id == anchor.line_id)
            })
            .or_else(|| self.capture_anchor(top))
    }

    fn capture_anchor(&self, top: Pixels) -> Option<ViewportAnchor> {
        if self.lines.is_empty() {
            return None;
        }
        let index = line_at_y(&self.prefix_rows, top, self.layout_key.row_height);
        let line = &self.lines[index];
        let geometry = line.geometry.as_ref()?;
        let local_y = (top - self.line_top(index)).max(px(0.));
        let row_index =
            ((local_y / self.layout_key.row_height).floor() as usize).min(geometry.rows.len() - 1);
        Some(ViewportAnchor {
            generation: self.identity?.0,
            line_id: line.id,
            byte: geometry.rows[row_index].bytes.start,
            affinity: RowAffinity::Downstream,
            intra_row_y: local_y - self.layout_key.row_height * row_index as f32,
        })
    }

    fn measure_line(
        &mut self,
        index: usize,
        background: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        if self.exact(index) {
            return false;
        }
        let key = self.layout_key;
        let style = self.style.as_ref().unwrap();
        let line = &mut self.lines[index];
        let mut cached = line.styled.borrow_mut();
        let mut styled = cached
            .styled
            .take()
            .unwrap_or_else(|| StyledText::new(cached.text.clone()));
        if !cached.styles.is_empty() {
            styled = styled.with_runs(text_runs(cached.text.len(), &cached.styles, style));
        }
        let mut text_style = style.text.subtract(&TextStyleRefinement::default());
        text_style.white_space = Some(if key.wrap_width.is_some() {
            WhiteSpace::Normal
        } else {
            WhiteSpace::Nowrap
        });
        window.with_text_style(Some(text_style), |window| {
            let (layout, ()) = styled.request_layout(None, None, window, cx);
            window.compute_layout(
                layout,
                size(
                    key.wrap_width
                        .map_or(AvailableSpace::MaxContent, AvailableSpace::Definite),
                    AvailableSpace::MaxContent,
                ),
                cx,
            );
            let measured = window.layout_bounds(layout).size;
            styled.prepaint(
                None,
                None,
                Bounds::new(point(px(0.), px(0.)), measured),
                &mut (),
                window,
                cx,
            );
        });
        let content_layout = styled.layout().line_layout_for_index(0).unwrap();
        line.width = Some(content_layout.unwrapped_layout.width.ceil());
        if let Some(geometry) = line.geometry.as_mut() {
            geometry.reflow(key, content_layout);
        } else {
            line.geometry = Some(LineGeometry::new(key, content_layout));
        }
        line.row_count = line.geometry.as_ref().unwrap().rows.len() as u32;
        if background {
            self.frame_work.background_lines += 1;
            self.frame_work.background_bytes += line.text.len();
        }
        cached.styled = Some(styled);
        true
    }

    fn measure_background(&mut self, window: &mut Window, cx: &mut App) {
        while self.reflow_cursor < self.lines.len() {
            let index = self.reflow_cursor;
            if self.exact(index) {
                self.reflow_cursor += 1;
                continue;
            }
            if !self.frame_work.permits(self.lines[index].text.len()) {
                break;
            }
            self.measure_line(index, true, window, cx);
            self.reflow_cursor += 1;
        }
    }

    pub(crate) fn settle_frame(
        &mut self,
        scroll: &ScrollHandle,
        visible: bool,
        soft_wrap: bool,
        follow: bool,
    ) -> bool {
        self.frame_queued = false;
        let Some(pending) = self.pending.take() else {
            return false;
        };
        if !visible
            || pending.generation != self.reflow_generation
            || pending.identity != self.identity
            || pending.revision != self.revision
            || pending.key != self.layout_key
            || pending.key.wrap_width.is_some() != soft_wrap
            || pending.follow != follow
            || scroll.offset() != pending.expected_offset
        {
            return false;
        }
        scroll.set_offset(pending.target_offset);
        if let Some(frame) = self.presented.as_mut() {
            frame.offset = pending.target_offset;
        }
        true
    }

    pub(crate) fn log_filter(&self) -> LogFilter {
        self.log_filter
    }

    pub(crate) fn set_log_filter(&mut self, filter: LogFilter) -> bool {
        if self.log_filter == filter {
            return false;
        }
        self.log_filter = filter;
        self.projection_dirty = true;
        self.invalidate_projection_selection();
        self.extent_dirty = true;
        self.reflow_cursor = 0;
        self.reflow_generation = self.reflow_generation.wrapping_add(1);
        true
    }

    fn invalidate_projection_selection(&mut self) {
        self.clear_selection();
        self.projection_generation = self.projection_generation.wrapping_add(1);
        self.content_key_epoch = self.content_key_epoch.wrapping_add(1);
        self.pending = None;
        self.hit_lines.clear();
        self.painted.clear();
        self.runs.clear();
    }

    fn completed_selection_before(&self, offset: usize) -> bool {
        self.completed
            && (self.selected.is_some() || self.endpoints.iter().any(Option::is_some))
            && self
                .selected
                .as_ref()
                .is_none_or(|range| range.end <= offset)
            && self.endpoints.iter().flatten().all(|byte| *byte <= offset)
    }

    /// Returns whether the owner must also clear Root's geometric selection.
    pub(crate) fn sync_buffer(&mut self, buffer: &TerminalBuffer, generation: u64) -> bool {
        let identity = (generation, buffer.selection_epoch());
        if self.identity == Some(identity)
            && self.revision == Some(buffer.revision())
            && !self.projection_dirty
        {
            return false;
        }
        let mut selection_invalidated = self.identity.is_some() && self.identity != Some(identity);
        if selection_invalidated {
            self.invalidate_projection_selection();
        }
        if self
            .identity
            .is_some_and(|previous| previous.0 != generation)
        {
            self.source_lines.clear();
            self.lines.clear();
            self.anchor = None;
            self.presented = None;
        }
        self.identity = Some(identity);
        self.revision = Some(buffer.revision());

        // Source IDs form a bounded ordered stream, even across u64 wrap. Walk
        // the old and new streams together rather than sorting IDs or cloning
        // text. The spare deque retains its allocation between synchronizations.
        let mut old_lines = std::mem::take(&mut self.lines);
        let mut lines = std::mem::take(&mut self.scratch_lines);
        lines.clear();
        let mut offset = 0;
        let mut count = 0;
        for (id, revision, text, styles, newline) in buffer.rows() {
            if count == 0 {
                let had_source = !self.source_lines.is_empty();
                while self.source_lines.front().is_some_and(|line| line.id != id) {
                    let removed = self.source_lines.pop_front().unwrap();
                    if self.projected_filter.includes(removed.level) {
                        old_lines.pop_front();
                    }
                }
                if had_source && self.source_lines.is_empty() {
                    self.anchor = None;
                    self.presented = None;
                }
            }
            let previous = self
                .source_lines
                .get(count)
                .copied()
                .filter(|line| line.id == id);
            let level = previous
                .filter(|line| line.revision == revision)
                .map_or_else(|| filter::classify(text), |line| line.level);
            if let Some(previous) = previous
                && !self.projection_dirty
                && self.projected_filter.includes(previous.level) != self.log_filter.includes(level)
                && !selection_invalidated
            {
                let changed_at = old_lines.front().map_or(self.byte_len, |line| line.offset);
                // Removing a zero-byte trailing row or changing a suffix after
                // a completed selection does not change any selected bytes or
                // content keys. An active gesture cannot make that guarantee.
                let removed_empty =
                    self.projected_filter.includes(previous.level) && previous.byte_len == 0;
                if !removed_empty && !self.completed_selection_before(changed_at) {
                    self.invalidate_projection_selection();
                    selection_invalidated = true;
                }
            }
            let source = SourceLine {
                id,
                revision,
                level,
                byte_len: text.len() + usize::from(newline),
            };
            if count == self.source_lines.len() {
                self.source_lines.push_back(source);
            } else {
                self.source_lines[count] = source;
            }

            let cached = previous
                .filter(|line| self.projected_filter.includes(line.level))
                .and_then(|_| old_lines.pop_front());
            if self.log_filter.includes(level) {
                let row = match cached {
                    Some(mut line) if line.id == id && line.revision == revision => {
                        line.offset = offset;
                        line
                    }
                    previous => {
                        let row_count = previous
                            .filter(|line| line.id == id)
                            .map_or(1, |line| line.row_count);
                        let mut row = String::with_capacity(text.len() + usize::from(newline));
                        row.push_str(text);
                        if newline {
                            row.push('\n');
                        }
                        let text: SharedString = row.into();
                        CachedLine {
                            id,
                            revision,
                            offset,
                            styled: Rc::new(RefCell::new(CachedText {
                                styled: None,
                                text: text.clone(),
                                styles: styles.to_vec(),
                            })),
                            text,
                            width: None,
                            row_count,
                            geometry: None,
                        }
                    }
                };
                offset += text.len() + usize::from(newline);
                lines.push_back(row);
            }
            count += 1;
        }
        self.source_lines.truncate(count);
        old_lines.clear();
        self.scratch_lines = old_lines;
        self.lines = lines;
        self.projected_filter = self.log_filter;
        self.projection_dirty = false;
        self.byte_len = offset;
        if self.anchor.is_none()
            && let Some(line) = self.lines.front()
        {
            self.anchor = Some(ViewportAnchor {
                generation,
                line_id: line.id,
                byte: 0,
                affinity: RowAffinity::Downstream,
                intra_row_y: px(0.),
            });
        }
        self.extent_dirty = true;
        self.rebuild_prefix();
        self.reflow_cursor = 0;
        self.reflow_generation = self.reflow_generation.wrapping_add(1);
        selection_invalidated
    }

    fn update_style(&mut self, style: RenderStyle) {
        if self.style.as_ref() != Some(&style) {
            for line in &mut self.lines {
                line.width = None;
                // StyledText keeps measured decorations even when new runs have
                // the same constraints; rebuild it from semantic styles lazily.
                line.styled.borrow_mut().styled = None;
            }
            self.extent_dirty = true;
            self.layout_key.style_generation = self.layout_key.style_generation.wrapping_add(1);
            self.layout_key.row_height = style.rem_size * ROW_HEIGHT_REM;
            if self
                .style
                .as_ref()
                .is_none_or(|previous| !previous.same_metrics(&style))
            {
                // Pixel columns cannot survive font/rem changes. Color-only
                // theme changes preserve the user's preferred visual column.
                self.preferred_x = None;
            }
            self.reflow_generation = self.reflow_generation.wrapping_add(1);
            self.reflow_cursor = 0;
            self.style = Some(style);
        }
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected = None;
        self.completed = false;
        self.endpoints = [None, None];
        self.caret_affinity = RowAffinity::Downstream;
        self.preferred_x = None;
        self.keyboard_caret = false;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.byte_len == 0
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        let range = self.selected.as_ref()?;
        if range.end > self.byte_len {
            return None;
        }
        let mut result = String::with_capacity(range.len());
        for line in &self.lines {
            let start = range.start.max(line.offset);
            let end = range.end.min(line.offset + line.text.len());
            if start < end {
                result.push_str(line.text.get(start - line.offset..end - line.offset)?);
            }
        }
        Some(result)
    }

    fn byte_at_point(&self, content_point: Point<Pixels>) -> Option<usize> {
        let frame = self.presented.as_ref()?;
        if frame.key != self.layout_key
            || frame.identity != self.identity
            || frame.projection_generation != self.projection_generation
            || frame.revision != self.revision
            || self.lines.is_empty()
        {
            return None;
        }
        if content_point.y < px(0.) {
            return Some(0);
        }
        if content_point.y >= frame.height {
            return Some(self.byte_len);
        }
        if content_point.x <= px(0.)
            && let Some(index) = self.hit_lines.iter().copied().find(|index| {
                let line = &self.lines[*index];
                line.text.ends_with('\n')
                    && content_point.y
                        == self.line_top(*index)
                            + self.layout_key.row_height * line.row_count as f32
            })
        {
            let line = &self.lines[index];
            return Some(line.offset + line.text.len());
        }
        let index = self
            .hit_lines
            .iter()
            .copied()
            .find(|index| {
                let top = self.line_top(*index);
                content_point.y >= top
                    && content_point.y
                        < top + self.layout_key.row_height * self.lines[*index].row_count as f32
            })
            .or_else(|| {
                if content_point.y < frame.viewport.top() - frame.origin.y {
                    Some(frame.visible.start)
                } else {
                    frame.visible.end.checked_sub(1)
                }
            })?;
        let line = self.lines.get(index)?;
        let geometry = line
            .geometry
            .as_ref()
            .filter(|geometry| geometry.key == frame.key)?;
        let y = (content_point.y - self.line_top(index)).clamp(
            px(0.),
            self.layout_key.row_height * line.row_count as f32 - px(0.01),
        );
        Some(line.offset + geometry.byte_at_point(&line.text, point(content_point.x, y)))
    }

    fn content_key(&self, offset: usize) -> TextSelectionContentKey {
        // The bounded core holds fewer than 2^32 displayed bytes. The upper
        // half makes an old key unusable when its projected offsets change.
        TextSelectionContentKey::new((u64::from(self.content_key_epoch) << 32) | offset as u64)
    }

    fn resolve_offset(
        &self,
        key: Option<TextSelectionContentKey>,
        position: Point<Pixels>,
    ) -> Option<usize> {
        if key.is_some_and(|key| key.value() >> 32 != u64::from(self.content_key_epoch)) {
            return None;
        }
        key.map(|key| (key.value() as u32) as usize)
            .filter(|offset| {
                self.lines.iter().any(|line| {
                    *offset >= line.offset
                        && *offset <= line.offset + line.text.len()
                        && line.text.is_char_boundary(*offset - line.offset)
                })
            })
            .or_else(|| self.byte_at_point(position))
    }

    pub(crate) fn project(&mut self, selection: &TextSelectionHandle, cx: &mut App) {
        // A completed selection remains fixed when live output extends the EOL.
        if self.completed {
            return;
        }
        let Some(snapshot) = selection.snapshot(cx) else {
            return;
        };
        let endpoint = |endpoint: gpui_kit::base::TextSelectionEndpoint| {
            self.resolve_offset(endpoint.content_key(), endpoint.content_point())
        };
        let anchor = endpoint(snapshot.anchor());
        let cursor = endpoint(snapshot.cursor());
        self.update_endpoints(anchor, cursor, snapshot.coverage(), snapshot.is_selecting());
    }

    fn update_endpoints(
        &mut self,
        anchor: Option<usize>,
        cursor: Option<usize>,
        coverage: TextSelectionCoverage,
        selecting: bool,
    ) {
        let anchor = anchor.or(self.endpoints[0]);
        let cursor = cursor.or(self.endpoints[1]);
        let (Some(anchor), Some(cursor)) = (anchor, cursor) else {
            return;
        };
        self.endpoints = [Some(anchor), Some(cursor)];
        let mut range = anchor.min(cursor)..anchor.max(cursor);
        match coverage {
            TextSelectionCoverage::Bounded => {}
            TextSelectionCoverage::FromStart => range.start = 0,
            TextSelectionCoverage::ToEnd => range.end = self.byte_len,
            TextSelectionCoverage::Full => range = 0..self.byte_len,
        }
        self.update_selection((!range.is_empty()).then_some(range), selecting);
    }

    fn update_selection(&mut self, selected: Option<Range<usize>>, selecting: bool) {
        if !self.completed {
            self.selected = selected;
            self.completed = !selecting;
        }
    }
}

#[derive(Clone, Copy)]
struct SourceLine {
    id: u64,
    revision: u64,
    byte_len: usize,
    level: Option<filter::LogLevel>,
}

struct CachedLine {
    id: u64,
    revision: u64,
    offset: usize,
    text: SharedString,
    styled: Rc<RefCell<CachedText>>,
    width: Option<Pixels>,
    row_count: u32,
    geometry: Option<LineGeometry>,
}

struct CachedText {
    text: SharedString,
    styles: Vec<StyleRun>,
    styled: Option<StyledText>,
}

struct PaintedLine {
    index: usize,
}

struct PresentedFrame {
    key: LayoutKey,
    identity: Option<(u64, u64)>,
    projection_generation: u64,
    revision: Option<u64>,
    origin: Point<Pixels>,
    viewport: Bounds<Pixels>,
    visible: Range<usize>,
    offset: Point<Pixels>,
    follow: bool,
    height: Pixels,
}

struct FrameCorrection {
    generation: u64,
    identity: Option<(u64, u64)>,
    revision: Option<u64>,
    key: LayoutKey,
    follow: bool,
    expected_offset: Point<Pixels>,
    target_offset: Point<Pixels>,
}

#[derive(Default)]
struct ReflowWork {
    background_lines: usize,
    background_bytes: usize,
}

impl ReflowWork {
    fn permits(&self, bytes: usize) -> bool {
        self.background_lines < REFLOW_LINES && self.background_bytes + bytes <= REFLOW_BYTES
    }
}

#[derive(Clone, PartialEq)]
struct RenderStyle {
    text: TextStyle,
    rem_size: Pixels,
    scale_factor: f32,
    background: Hsla,
}

impl RenderStyle {
    fn same_metrics(&self, other: &Self) -> bool {
        self.rem_size == other.rem_size
            && self.scale_factor == other.scale_factor
            && self.text.font_family == other.text.font_family
            && self.text.font_features == other.text.font_features
            && self.text.font_fallbacks == other.text.font_fallbacks
            && self.text.font_size == other.text.font_size
            && self.text.line_height == other.text.line_height
            && self.text.font_weight == other.text.font_weight
            && self.text.font_style == other.text.font_style
            && self.text.white_space == other.text.white_space
            && self.text.text_overflow == other.text.text_overflow
            && self.text.text_align == other.text.text_align
            && self.text.line_clamp == other.text.line_clamp
    }
}

// Kit has no terminal palette. Keep the familiar ANSI hues, with separate
// readable foreground shades for light and dark terminal surfaces. Extended
// xterm and truecolor values below are never theme-adjusted.
const ANSI_DARK: [u32; 16] = [
    0x666666, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf, 0x909090,
    0xff8b92, 0xb5e890, 0xffdd9a, 0x8acbff, 0xe6a3f5, 0x80dce8, 0xffffff,
];
const ANSI_LIGHT: [u32; 16] = [
    0x242424, 0xb42332, 0x28752a, 0x856000, 0x195bb5, 0x8834a5, 0x007580, 0x626262, 0x707070,
    0xc32d3b, 0x33812d, 0x936900, 0x246aca, 0x9844b4, 0x00818e, 0x767676,
];

fn indexed_color(index: u8, light: bool) -> Hsla {
    let value = match index {
        0..=15 => (if light { ANSI_LIGHT } else { ANSI_DARK })[usize::from(index)],
        16..=231 => {
            let index = u32::from(index - 16);
            let component = |value| if value == 0 { 0 } else { 55 + value * 40 };
            (component(index / 36) << 16) | (component(index / 6 % 6) << 8) | component(index % 6)
        }
        232..=255 => {
            let value = 8 + u32::from(index - 232) * 10;
            (value << 16) | (value << 8) | value
        }
    };
    rgb(value).into()
}

fn resolve_color(color: AnsiColor, default: Hsla, background: Hsla) -> Hsla {
    match color {
        AnsiColor::Default => default,
        AnsiColor::Indexed(index) => indexed_color(index, background.l > 0.5),
        AnsiColor::Rgb(red, green, blue) => {
            rgb((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)).into()
        }
    }
}

fn styled_run(len: usize, ansi: AnsiStyle, style: &RenderStyle) -> TextRun {
    let mut run = style.text.to_run(len);
    let mut foreground = resolve_color(ansi.foreground, style.text.color, style.background);
    let mut background = resolve_color(ansi.background, style.background, style.background);
    if ansi.inverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    if ansi.dim {
        let foreground_rgb: Rgba = foreground.into();
        let background_rgb: Rgba = background.into();
        foreground = Rgba {
            r: (foreground_rgb.r + background_rgb.r) * 0.5,
            g: (foreground_rgb.g + background_rgb.g) * 0.5,
            b: (foreground_rgb.b + background_rgb.b) * 0.5,
            a: foreground_rgb.a,
        }
        .into();
    }
    run.color = foreground;
    run.background_color =
        (ansi.inverse || ansi.background != AnsiColor::Default).then_some(background);
    if ansi.bold {
        run.font.weight = FontWeight::BOLD;
    }
    run
}

fn text_runs(len: usize, styles: &[StyleRun], style: &RenderStyle) -> Vec<TextRun> {
    let mut runs = Vec::with_capacity(styles.len() * 2 + 1);
    let mut offset = 0;
    for span in styles {
        if offset < span.range.start {
            runs.push(style.text.to_run(span.range.start - offset));
        }
        runs.push(styled_run(span.range.len(), span.style, style));
        offset = span.range.end;
    }
    if offset < len {
        runs.push(style.text.to_run(len - offset));
    }
    runs
}

type ReflowCallback = Rc<dyn Fn(&mut Window, &mut App)>;

pub(crate) struct TranscriptViewport {
    pub soft_wrap: bool,
    pub follow: bool,
    pub scroll: ScrollHandle,
    pub focus: FocusHandle,
    pub on_reflow: ReflowCallback,
}

pub(crate) struct TerminalTranscript {
    id: ElementId,
    selection: TextSelectionHandle,
    state: Rc<RefCell<TranscriptState>>,
    selection_color: Hsla,
    background: Hsla,
    viewport: TranscriptViewport,
}

impl TerminalTranscript {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        selection: TextSelectionHandle,
        state: Rc<RefCell<TranscriptState>>,
        selection_color: Hsla,
        background: Hsla,
        viewport: TranscriptViewport,
    ) -> Self {
        Self {
            id: id.into(),
            selection,
            state,
            selection_color,
            background,
            viewport,
        }
    }
}

impl IntoElement for TerminalTranscript {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

fn nearest_character_boundary(
    index: usize,
    next_index: usize,
    pointer_x: Pixels,
    start_x: Pixels,
    width: Pixels,
) -> usize {
    if pointer_x > start_x + width / 2. {
        next_index
    } else {
        index
    }
}

impl Element for TerminalTranscript {
    type RequestLayoutState = Point<Pixels>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut state = self.state.borrow_mut();
        state.update_style(RenderStyle {
            text: window.text_style(),
            rem_size: window.rem_size(),
            scale_factor: window.scale_factor(),
            background: self.background,
        });
        let width = if self.viewport.soft_wrap {
            px(0.)
        } else {
            state.width
        };
        let height = state.height();
        let layout = window.request_measured_layout(
            Style {
                flex_shrink: 0.,
                min_size: size(width.into(), height.into()),
                ..Default::default()
            },
            move |known, available, _, _| {
                let available_width = match available.width {
                    AvailableSpace::Definite(width) => width,
                    _ => px(0.),
                };
                size(known.width.unwrap_or(available_width).max(width), height)
            },
        );
        (layout, self.viewport.scroll.offset())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        requested_offset: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut state = self.state.borrow_mut();
        // Fast may expand the content mask for a scroll layer's paint cache.
        // Only the tracked owner bounds define logical scrolling and navigation.
        let viewport = self.viewport.scroll.bounds();
        let paint_region = window.content_mask().bounds;
        let offset = self.viewport.scroll.offset();
        let unscrolled_origin = bounds.origin - window.pixel_snap_point(offset);
        // A newly enabled Follow is an explicit request even when no text or
        // geometry changed; later frames must still preserve user scrolling.
        let follow_resumed =
            self.viewport.follow && state.presented.as_ref().is_some_and(|frame| !frame.follow);
        let presented_top = state.presented_top(*requested_offset);
        let prior_anchor = if state
            .presented
            .as_ref()
            .is_none_or(|frame| frame.offset.y == requested_offset.y)
        {
            state.anchor
        } else {
            presented_top.and_then(|top| state.capture_anchor(top))
        };
        let wrap_width = self
            .viewport
            .soft_wrap
            .then_some(bounds.size.width.max(px(1.)));
        let key_changed = state.set_wrap_width(wrap_width);
        let viewport_changed = state
            .presented
            .as_ref()
            .is_none_or(|frame| frame.viewport.size != viewport.size);
        let changed = key_changed || state.extent_dirty || viewport_changed;
        state.frame_work = ReflowWork::default();
        state.painted.clear();
        state.runs.clear();
        state.hit_lines.clear();
        let mut geometry_changed = changed;
        if !state.lines.is_empty() {
            let last = state.lines.len() - 1;
            geometry_changed |= state.measure_line(0, false, window, cx);
            geometry_changed |= state.measure_line(last, false, window, cx);
            if let Some(index) = prior_anchor.and_then(|anchor| state.anchor_index(anchor)) {
                geometry_changed |= state.measure_line(index, false, window, cx);
            }
            for endpoint in state.endpoints.into_iter().flatten() {
                if let Some(index) = state.lines.iter().position(|line| {
                    endpoint >= line.offset && endpoint <= line.offset + line.text.len()
                }) {
                    geometry_changed |= state.measure_line(index, false, window, cx);
                    state.hit_lines.push(index);
                }
            }
        }
        state.measure_background(window, cx);
        if geometry_changed || state.frame_work.background_lines != 0 {
            state.rebuild_prefix();
        }
        geometry_changed |= state.frame_work.background_lines != 0;
        let mut top =
            presented_top.unwrap_or(viewport.top() - unscrolled_origin.y - requested_offset.y);
        let mut rows;
        let mut paint_rows;
        let mut target_offset;
        let mut origin;
        loop {
            top = state.viewport_top(
                prior_anchor,
                top,
                viewport.size.height,
                self.viewport.follow,
                geometry_changed || follow_resumed,
            );
            target_offset = point(
                if self.viewport.soft_wrap {
                    px(0.)
                } else {
                    offset.x
                },
                viewport.top() - unscrolled_origin.y - top,
            );
            origin = unscrolled_origin + window.pixel_snap_point(target_offset);
            rows = visible_lines(
                &state.prefix_rows,
                viewport.top() - origin.y,
                viewport.bottom() - origin.y,
                state.layout_key.row_height,
            );
            paint_rows = visible_lines(
                &state.prefix_rows,
                paint_region.top() - origin.y,
                paint_region.bottom() - origin.y,
                state.layout_key.row_height,
            );
            let mut measured = false;
            let overscan = rows.start.min(paint_rows.start).saturating_sub(1)
                ..(rows.end.max(paint_rows.end) + 1).min(state.lines.len());
            for index in overscan {
                if !state.exact(index) {
                    state.measure_line(index, false, window, cx);
                    measured = true;
                }
            }
            if !measured {
                break;
            }
            geometry_changed = true;
            state.rebuild_prefix();
        }
        let document_bounds = Bounds::new(
            origin,
            size(
                if self.viewport.soft_wrap {
                    bounds.size.width
                } else {
                    bounds.size.width.max(state.width)
                },
                state.height(),
            ),
        );
        state.anchor = state.viewport_anchor(prior_anchor, top, self.viewport.follow);
        state.hit_lines.extend(rows.clone());
        state.hit_lines.extend(paint_rows.clone());
        if !state.lines.is_empty() {
            state.hit_lines.push(0);
            let last = state.lines.len() - 1;
            state.hit_lines.push(last);
        }
        state.hit_lines.sort_unstable();
        state.hit_lines.dedup();
        for ordinal in 0..state.hit_lines.len() {
            let index = state.hit_lines[ordinal];
            let line = &state.lines[index];
            let row_bounds = Bounds::new(
                origin + point(px(0.), state.line_top(index)),
                size(
                    document_bounds.size.width,
                    state.layout_key.row_height * line.row_count as f32,
                ),
            );
            let mut cached = line.styled.borrow_mut();
            cached
                .styled
                .as_mut()
                .unwrap()
                .prepaint(None, None, row_bounds, &mut (), window, cx);
            let run = TextSelectionRun::new(
                line.text.clone(),
                cached.styled.as_ref().unwrap().layout().clone(),
                row_bounds,
            )
            .with_document_order(line.offset as u64);
            drop(cached);
            state.runs.push(run);
            if paint_rows.contains(&index) {
                state.painted.push(PaintedLine { index });
            }
        }
        state.presented = Some(PresentedFrame {
            key: state.layout_key,
            identity: state.identity,
            projection_generation: state.projection_generation,
            revision: state.revision,
            origin,
            viewport,
            visible: rows,
            offset,
            follow: self.viewport.follow,
            height: state.height(),
        });
        state.extent_dirty = false;
        let needs_frame = geometry_changed || target_offset != offset;
        if needs_frame {
            state.pending = Some(FrameCorrection {
                generation: state.reflow_generation,
                identity: state.identity,
                revision: state.revision,
                key: state.layout_key,
                follow: self.viewport.follow,
                expected_offset: offset,
                target_offset,
            });
            if !state.frame_queued {
                state.frame_queued = true;
                let callback = self.viewport.on_reflow.clone();
                window.on_next_frame(move |window, cx| callback(window, cx));
            }
        }
        if !state.callbacks_installed {
            let weak = Rc::downgrade(&self.state);
            self.selection.resolve_content_key_with(
                move |position, _| {
                    let state = weak.upgrade()?;
                    let state = state.borrow();
                    let offset = state.byte_at_point(position)?;
                    Some(state.content_key(offset))
                },
                cx,
            );
            let weak = Rc::downgrade(&self.state);
            self.selection.copy_with(
                move |_| {
                    weak.upgrade()
                        .and_then(|state| state.borrow().selected_text())
                        .unwrap_or_default()
                },
                cx,
            );
            state.callbacks_installed = true;
        }
        drop(state);
        let hitbox = window.insert_hitbox(document_bounds, HitboxBehavior::Normal);
        self.selection.register(
            TextSelectionRegistration::new(hitbox, document_bounds)
                .with_text_bounds(vec![document_bounds])
                .with_rendered_element(&self.selection, window, cx),
            window,
            cx,
        );
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut state = self.state.borrow_mut();
        self.selection.update_runs(&state.runs, cx);
        state.project(&self.selection, cx);
        let clip = window.content_mask().bounds;
        for painted in &state.painted {
            let line = &state.lines[painted.index];
            let cached = line.styled.borrow();
            let layout = cached.styled.as_ref().unwrap().layout();
            layout
                .paint_background(window, cx)
                .expect("failed to paint terminal text backgrounds");
            if let Some(selected) = &state.selected {
                let start = selected.start.max(line.offset);
                let end = selected.end.min(line.offset + line.text.len());
                if start < end {
                    let geometry = line.geometry.as_ref().unwrap();
                    for (row, x) in geometry.selection_rows(
                        start - line.offset..end - line.offset,
                        line.text.ends_with('\n'),
                        layout.bounds().size.width,
                    ) {
                        let origin = layout.bounds().origin
                            + point(px(0.), state.layout_key.row_height * row as f32);
                        if origin.y >= clip.bottom() {
                            break;
                        }
                        if origin.y + state.layout_key.row_height <= clip.top() {
                            continue;
                        }
                        window.paint_quad(fill(
                            Bounds::from_corners(
                                origin + point(x.start, px(0.)),
                                origin + point(x.end, state.layout_key.row_height),
                            ),
                            self.selection_color,
                        ));
                    }
                }
            }
            layout
                .paint_foreground(window, cx)
                .expect("failed to paint terminal text foreground");
        }
        if state.keyboard_caret
            && self.viewport.focus.is_focused(window)
            && let Some(byte) = state.endpoints[1]
            && let Some(position) = state.caret_position(byte, state.caret_affinity)
        {
            // One physical pixel keeps the readonly caret crisp at fractional DPI.
            let width = px(1. / window.scale_factor());
            let origin = state.presented.as_ref().unwrap().origin + position;
            window.paint_quad(fill(
                Bounds::new(origin, size(width, state.layout_key.row_height)),
                state.style.as_ref().unwrap().text.color,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::geometry::{RowAffinity, geometry_fixture};
    use super::{
        FrameCorrection, LogFilter, PresentedFrame, RenderStyle, TranscriptState, indexed_color,
        styled_run, text_runs,
    };
    use gpui_kit::base::{TextSelectionContentKey, TextSelectionCoverage};
    use gpui_kit::{Bounds, FontWeight, Hsla, Rgba, ScrollHandle, TextStyle, point, px, rgb, size};
    use slimenrf_terminal_core::{AnsiColor, AnsiStyle, MAX_LINES, StyleRun, TerminalBuffer};
    use std::rc::Rc;

    fn set_geometry(state: &mut TranscriptState, index: usize, seams: &[usize]) {
        let line = &mut state.lines[index];
        let mut geometry = geometry_fixture(&line.text, seams);
        geometry.key = state.layout_key;
        line.row_count = geometry.rows.len() as u32;
        line.width = Some(geometry.content_layout.unwrapped_layout.width);
        line.geometry = Some(geometry);
    }

    fn present(state: &mut TranscriptState, visible: std::ops::Range<usize>, top: f32) {
        state.anchor = state.capture_anchor(px(top));
        state.hit_lines = visible.clone().collect();
        state.presented = Some(PresentedFrame {
            key: state.layout_key,
            identity: state.identity,
            projection_generation: state.projection_generation,
            revision: state.revision,
            origin: point(px(0.), px(-top)),
            viewport: Bounds::new(point(px(0.), px(0.)), size(px(100.), px(40.))),
            visible,
            offset: point(px(0.), px(-top)),
            follow: false,
            height: state.height(),
        });
    }

    fn correction(state: &TranscriptState, expected: f32, target: f32) -> FrameCorrection {
        FrameCorrection {
            generation: state.reflow_generation,
            identity: state.identity,
            revision: state.revision,
            key: state.layout_key,
            follow: false,
            expected_offset: point(px(0.), px(expected)),
            target_offset: point(px(0.), px(target)),
        }
    }

    fn projected_text(state: &TranscriptState) -> String {
        state.lines.iter().map(|line| line.text.as_ref()).collect()
    }

    #[test]
    fn exact_level_projection_keeps_unclassified_rows_and_the_full_source() {
        let text = concat!(
            "command\n",
            "<err> app: fail\n",
            "continuation\n",
            "<wrn> radio: warn\n",
            "\n",
            "[12] <inf> module: info\n",
            "<dbg> app_2: debug\n",
            "[display dropped 42 bytes; identity parsing continued]\n",
            "notice"
        );
        let mut buffer = TerminalBuffer::new();
        buffer.push(text.as_bytes());
        let mut state = TranscriptState::default();
        assert_eq!(state.log_filter(), LogFilter::All);
        assert!(!state.sync_buffer(&buffer, 1));
        assert_eq!(projected_text(&state), text);
        for (filter, classified) in [
            (LogFilter::Error, "<err> app: fail\n"),
            (LogFilter::Warning, "<wrn> radio: warn\n"),
            (LogFilter::Info, "[12] <inf> module: info\n"),
            (LogFilter::Debug, "<dbg> app_2: debug\n"),
        ] {
            assert!(state.set_log_filter(filter));
            assert!(!state.set_log_filter(filter));
            assert!(!state.sync_buffer(&buffer, 1));
            let prefix = match filter {
                LogFilter::Error => format!("command\n{classified}continuation\n\n"),
                LogFilter::Warning => format!("command\ncontinuation\n{classified}\n"),
                LogFilter::Info | LogFilter::Debug => {
                    format!("command\ncontinuation\n\n{classified}")
                }
                LogFilter::All => unreachable!(),
            };
            let expected =
                format!("{prefix}[display dropped 42 bytes; identity parsing continued]\nnotice");
            assert_eq!(projected_text(&state), expected, "{filter:?}");
            assert_eq!(state.byte_len, expected.len());
            assert_eq!(buffer.text(), text);
        }
        assert!(state.set_log_filter(LogFilter::All));
        assert!(!state.sync_buffer(&buffer, 1));
        assert_eq!(projected_text(&state), text);

        let mut only_hidden = TerminalBuffer::new();
        only_hidden.push(b"<inf> app: incomplete last line");
        state.sync_buffer(&only_hidden, 2);
        state.set_log_filter(LogFilter::Error);
        state.sync_buffer(&only_hidden, 2);
        assert!(state.lines.is_empty());
        assert!(state.is_empty());
        assert_eq!(only_hidden.text(), "<inf> app: incomplete last line");
    }

    #[test]
    fn filtered_utf8_selection_skips_hidden_gaps_and_freezes_across_append() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("世界\n<inf> app: hidden\n<err> app: 🦀\n\nok".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        state.set_log_filter(LogFilter::Error);
        state.sync_buffer(&buffer, 1);
        let visible = "世界\n<err> app: 🦀\n\nok";
        assert_eq!(projected_text(&state), visible);
        assert_eq!(
            state
                .lines
                .iter()
                .map(|line| line.offset)
                .collect::<Vec<_>>(),
            vec![
                0,
                "世界\n".len(),
                "世界\n<err> app: 🦀\n".len(),
                visible.len() - 2
            ]
        );
        let start = "世".len();
        let end = visible.len() - 2;
        state.update_endpoints(
            Some(end),
            Some(start),
            TextSelectionCoverage::Bounded,
            false,
        );
        assert_eq!(
            state.selected_text().as_deref(),
            Some("界\n<err> app: 🦀\n\n")
        );
        set_geometry(&mut state, 0, &[]);
        let stable = state.lines[0].styled.clone();
        buffer.push(b" appended");
        assert!(!state.sync_buffer(&buffer, 1));
        assert_eq!(
            state.selected_text().as_deref(),
            Some("界\n<err> app: 🦀\n\n")
        );
        assert!(Rc::ptr_eq(&stable, &state.lines[0].styled));
        assert!(state.lines[0].geometry.is_some());
        state.set_log_filter(LogFilter::Debug);
        assert_eq!(state.selected_text(), None);
        state.sync_buffer(&buffer, 1);
        assert_eq!(projected_text(&state), "世界\n\nok appended");
        assert!(Rc::ptr_eq(&stable, &state.lines[0].styled));
        assert!(state.lines[0].geometry.is_some());
        state.set_log_filter(LogFilter::All);
        state.sync_buffer(&buffer, 1);
        assert!(Rc::ptr_eq(&stable, &state.lines[0].styled));
    }

    #[test]
    fn streaming_partial_tags_and_carriage_returns_reconcile_membership() {
        let mut buffer = TerminalBuffer::new();
        buffer.push(b"\x1b[31mplain response\x1b[0m\n\x1b[32m<err> app: failure\x1b[0m\n");
        let mut state = TranscriptState::default();
        state.set_log_filter(LogFilter::Error);
        state.sync_buffer(&buffer, 1);
        state.update_endpoints(Some(0), Some(5), TextSelectionCoverage::Bounded, false);
        let original_epoch = buffer.selection_epoch();
        let key = state.content_key(0);
        buffer.push(b"<inf> app: hidden");
        // Replacing the empty trailing row with a hidden line cannot change
        // any existing byte endpoint.
        assert!(!state.sync_buffer(&buffer, 1));
        assert_eq!(state.selected_text().as_deref(), Some("plain"));
        assert_eq!(
            projected_text(&state),
            "plain response\n<err> app: failure\n"
        );
        buffer.push(b"\n<i");
        assert!(!state.sync_buffer(&buffer, 1));
        assert_eq!(state.selected_text().as_deref(), Some("plain"));
        buffer.push(b"nf> app: hidden partial");
        assert_eq!(buffer.selection_epoch(), original_epoch);
        assert!(!state.sync_buffer(&buffer, 1));
        assert_eq!(state.selected_text().as_deref(), Some("plain"));
        assert_eq!(
            state.resolve_offset(Some(key), point(px(-1.), px(-1.))),
            Some(0)
        );
        assert_eq!(
            projected_text(&state),
            "plain response\n<err> app: failure\n"
        );

        buffer.push(b"\n<err> app: visible");
        assert!(!state.sync_buffer(&buffer, 1));
        assert_eq!(state.selected_text().as_deref(), Some("plain"));
        assert_eq!(
            state.resolve_offset(Some(key), point(px(-1.), px(-1.))),
            Some(0)
        );
        buffer.push(b"\n<i");
        assert!(!state.sync_buffer(&buffer, 1));
        state.clear_selection();
        state.update_endpoints(
            Some(state.byte_len - 2),
            Some(state.byte_len),
            TextSelectionCoverage::Bounded,
            false,
        );
        assert_eq!(state.selected_text().as_deref(), Some("<i"));
        let selected_tail_key = state.content_key(state.byte_len - 2);
        buffer.push(b"nf> app: partial");
        assert_eq!(buffer.selection_epoch(), original_epoch);
        assert!(state.sync_buffer(&buffer, 1));
        assert_eq!(state.selected_text(), None);
        assert_eq!(
            state.resolve_offset(Some(selected_tail_key), point(px(-1.), px(-1.))),
            None
        );
        assert_eq!(
            projected_text(&state),
            "plain response\n<err> app: failure\n<err> app: visible\n"
        );

        buffer.push(b"\r<err>");
        assert!(state.sync_buffer(&buffer, 1));
        assert_eq!(
            projected_text(&state),
            "plain response\n<err> app: failure\n<err> app: visible\n<err> app: partial"
        );
    }

    #[test]
    fn trimming_hidden_source_rows_reuses_visible_caches_and_invalidates_offsets() {
        let mut buffer = TerminalBuffer::new();
        for index in 0..MAX_LINES - 1 {
            if index % 2 == 0 {
                buffer.push(b"<inf> app: hidden\n");
            } else {
                buffer.push(format!("visible {index}\n").as_bytes());
            }
        }
        let mut state = TranscriptState::default();
        state.set_log_filter(LogFilter::Error);
        state.sync_buffer(&buffer, 1);
        let surviving = state.lines[0].styled.clone();
        let surviving_id = state.lines[0].id;
        state.update_endpoints(Some(0), Some(7), TextSelectionCoverage::Bounded, false);
        buffer.push(b"last\n");
        assert!(state.sync_buffer(&buffer, 1));
        assert_eq!(state.selected_text(), None);
        assert_eq!(state.lines[0].id, surviving_id);
        assert!(Rc::ptr_eq(&surviving, &state.lines[0].styled));
        assert_eq!(state.lines[0].offset, 0);
        assert_eq!(state.source_lines.len(), MAX_LINES);
    }

    #[test]
    fn filter_change_rejects_old_keys_and_corrections_but_preserves_visible_anchor() {
        let mut buffer = TerminalBuffer::new();
        buffer.push(b"<err> m: A\n<inf> m: B\n<err> m: C\nuntagged\ntail");
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        for index in 0..state.lines.len() {
            set_geometry(&mut state, index, &[]);
        }
        state.rebuild_prefix();
        present(&mut state, 2..4, 45.);
        let anchor = state.anchor.unwrap();
        let key = state.content_key(state.lines[2].offset);
        state.update_endpoints(Some(0), Some(4), TextSelectionCoverage::Bounded, false);
        state.pending = Some(correction(&state, -45., -60.));
        assert!(state.set_log_filter(LogFilter::Error));
        assert!(state.pending.is_none());
        assert_eq!(state.selected_text(), None);
        assert_eq!(state.byte_at_point(point(px(0.), px(1.))), None);
        assert_eq!(state.resolve_offset(Some(key), point(px(0.), px(1.))), None);
        assert!(!state.sync_buffer(&buffer, 1));
        assert_eq!(state.anchor_top(anchor), Some(px(25.)));
        assert_eq!(
            state.viewport_top(Some(anchor), px(0.), px(40.), false, true),
            px(25.)
        );
        assert_eq!(
            state.viewport_top(Some(anchor), px(0.), px(40.), true, true),
            px(40.)
        );
        assert_eq!(state.byte_at_point(point(px(0.), px(1.))), None);
        let scroll = ScrollHandle::default();
        assert!(!state.settle_frame(&scroll, true, false, false));

        state.set_log_filter(LogFilter::Info);
        state.sync_buffer(&buffer, 1);
        set_geometry(&mut state, 0, &[]);
        state.rebuild_prefix();
        assert_eq!(state.anchor_index(anchor), Some(0));
        assert_eq!(state.anchor_top(anchor), Some(px(5.)));
    }

    #[test]
    fn soft_row_selections_copy_logical_utf8_in_both_directions_and_freeze_across_reflow() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("a界世界 🦀\n\nend".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 7);
        set_geometry(&mut state, 0, &[4, 11]);
        set_geometry(&mut state, 1, &[]);
        set_geometry(&mut state, 2, &[]);
        state.rebuild_prefix();
        for (anchor, cursor) in [(1, 15), (15, 1)] {
            state.clear_selection();
            state.update_endpoints(
                Some(anchor),
                Some(cursor),
                TextSelectionCoverage::Bounded,
                false,
            );
            assert_eq!(state.selected_text().as_deref(), Some("界世界 🦀"));
        }
        state.layout_key.wrap_width = Some(px(10.));
        set_geometry(&mut state, 0, &[1, 4, 7, 10, 11]);
        state.rebuild_prefix();
        state.update_endpoints(
            Some(0),
            Some(state.byte_len),
            TextSelectionCoverage::Full,
            false,
        );
        assert_eq!(state.selected_text().as_deref(), Some("界世界 🦀"));
        state.clear_selection();
        state.update_endpoints(
            Some(1),
            Some(state.byte_len),
            TextSelectionCoverage::Bounded,
            false,
        );
        assert_eq!(state.selected_text().as_deref(), Some("界世界 🦀\n\nend"));
    }

    #[test]
    fn document_height_counts_content_wrap_rows_and_real_empty_lines_once() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("a界世界 🦀\n\n".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        set_geometry(&mut state, 0, &[4, 11]);
        set_geometry(&mut state, 1, &[]);
        set_geometry(&mut state, 2, &[]);
        state.rebuild_prefix();
        assert_eq!(state.height(), px(100.));
        assert_eq!(state.line_top(1), px(60.));
        assert_eq!(state.line_top(2), px(80.));
        state.update_endpoints(Some(15), Some(17), TextSelectionCoverage::Bounded, false);
        assert_eq!(state.selected_text().as_deref(), Some("\n\n"));
    }

    #[test]
    fn logical_anchor_survives_wrap_seams_prefix_corrections_and_follow_changes() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("head\na界世界 🦀\nlast".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 3);
        set_geometry(&mut state, 0, &[]);
        set_geometry(&mut state, 1, &[4, 11]);
        set_geometry(&mut state, 2, &[]);
        state.rebuild_prefix();
        let anchor = state.capture_anchor(px(43.)).unwrap();
        assert_eq!(anchor.byte, 4);
        assert_eq!(anchor.affinity, RowAffinity::Downstream);
        assert_eq!(state.anchor_top(anchor), Some(px(43.)));
        state.layout_key.wrap_width = Some(px(10.));
        set_geometry(&mut state, 0, &[1, 2, 3]);
        set_geometry(&mut state, 1, &[1, 4, 7, 10, 11]);
        set_geometry(&mut state, 2, &[]);
        state.rebuild_prefix();
        assert_eq!(state.anchor_top(anchor), Some(px(123.)));
        assert_eq!(
            state.viewport_top(Some(anchor), px(0.), px(40.), false, true),
            px(123.)
        );
        assert_eq!(
            state.viewport_top(Some(anchor), px(0.), px(40.), true, true),
            px(180.)
        );
        assert_eq!(
            state.viewport_top(Some(anchor), px(0.), px(40.), true, false),
            px(123.)
        );
        state.layout_key.wrap_width = None;
        set_geometry(&mut state, 0, &[]);
        set_geometry(&mut state, 1, &[]);
        set_geometry(&mut state, 2, &[]);
        state.rebuild_prefix();
        assert_eq!(state.anchor_top(anchor), Some(px(23.)));
        let preserved = state.viewport_anchor(Some(anchor), px(23.), false).unwrap();
        assert_eq!(preserved.byte, 4);
        state.set_wrap_width(Some(px(10.)));
        set_geometry(&mut state, 0, &[1, 2, 3]);
        set_geometry(&mut state, 1, &[1, 4, 7, 10, 11]);
        state.rebuild_prefix();
        assert_eq!(state.anchor_top(preserved), Some(px(123.)));
    }

    #[test]
    fn head_trim_preserves_surviving_anchor_and_removed_anchor_clamps_before_epoch_reset() {
        let mut buffer = TerminalBuffer::new();
        for _ in 0..MAX_LINES - 1 {
            buffer.push(b"abc\n");
        }
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        set_geometry(&mut state, 0, &[1]);
        set_geometry(&mut state, 1, &[1]);
        state.rebuild_prefix();
        let removed = state.capture_anchor(px(1.)).unwrap();
        let surviving = state.capture_anchor(px(61.)).unwrap();
        state.anchor = Some(surviving);
        present(&mut state, 1..2, 61.);
        buffer.push(b"next\n");
        state.sync_buffer(&buffer, 1);
        assert_eq!(state.anchor_top(surviving), Some(px(21.)));
        assert_eq!(state.anchor_top(removed), Some(px(1.)));
        assert_eq!(state.presented_top(point(px(0.), px(-71.))), Some(px(31.)));
        state.sync_buffer(&buffer, 2);
        assert_eq!(state.anchor_top(surviving), None);
        assert!(state.capture_anchor(px(0.)).is_none());
        buffer.clear();
        buffer.push(b"fresh\nline");
        state.sync_buffer(&buffer, 2);
        set_geometry(&mut state, 0, &[]);
        set_geometry(&mut state, 1, &[]);
        state.rebuild_prefix();
        assert_eq!(
            state.viewport_top(state.anchor, px(500.), px(20.), false, true),
            px(0.)
        );
    }

    #[test]
    fn stale_or_provisional_hit_geometry_never_creates_keys_and_unresolved_drag_keeps_endpoint() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("a界世界 🦀\nunknown\nlast".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        set_geometry(&mut state, 0, &[4, 11]);
        state.rebuild_prefix();
        present(&mut state, 0..1, 0.);
        assert_eq!(state.byte_at_point(point(px(6.), px(21.))), Some(7));
        assert_eq!(state.byte_at_point(point(px(16.), px(41.))), Some(16));
        assert_eq!(state.byte_at_point(point(px(0.), px(60.))), Some(16));
        state.update_endpoints(Some(1), Some(11), TextSelectionCoverage::Bounded, true);
        state.layout_key.wrap_width = Some(px(10.));
        assert_eq!(state.byte_at_point(point(px(6.), px(21.))), None);
        state.update_endpoints(None, Some(15), TextSelectionCoverage::Bounded, true);
        assert_eq!(state.selected_text().as_deref(), Some("界世界 🦀"));
        assert_eq!(
            state.resolve_offset(Some(TextSelectionContentKey::new(2)), point(px(0.), px(0.))),
            None
        );
        buffer.push(b"\rnew");
        state.sync_buffer(&buffer, 1);
        assert_eq!(state.selected_text(), None);
        assert_eq!(state.byte_at_point(point(px(6.), px(21.))), None);
    }

    #[test]
    fn deferred_correction_preserves_intervening_wheel_delta_and_rejects_old_generations() {
        let mut buffer = TerminalBuffer::new();
        buffer.push(b"one\ntwo\nthree\nfour\nfive\nsix");
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        for index in 0..state.lines.len() {
            set_geometry(&mut state, index, &[]);
        }
        state.rebuild_prefix();
        present(&mut state, 2..4, 40.);
        state.presented.as_mut().unwrap().offset.y = px(-20.);
        let scroll = ScrollHandle::default();
        scroll.set_offset(point(px(0.), px(-30.)));
        state.pending = Some(correction(&state, -20., -40.));
        assert!(!state.settle_frame(&scroll, true, false, false));
        assert_eq!(scroll.offset().y, px(-30.));
        assert_eq!(state.presented_top(scroll.offset()), Some(px(50.)));
        state.pending = Some(correction(&state, -30., -50.));
        assert!(state.settle_frame(&scroll, true, false, false));
        assert_eq!(state.presented_top(scroll.offset()), Some(px(40.)));
        state.pending = Some(correction(&state, -50., -60.));
        state.set_wrap_width(Some(px(80.)));
        assert!(!state.settle_frame(&scroll, true, false, false));
        assert_eq!(scroll.offset().y, px(-50.));
        state.pending = Some(correction(&state, -50., -60.));
        assert!(!state.settle_frame(&scroll, false, true, false));
        assert_eq!(scroll.offset().y, px(-50.));
    }

    #[test]
    fn background_reflow_stops_at_line_or_byte_boundary_and_width_storms_supersede_work() {
        use super::ReflowWork;

        for (bytes, expected) in [(1, 8), (8192, 4), (8193, 3)] {
            let mut work = ReflowWork::default();
            while work.permits(bytes) {
                work.background_lines += 1;
                work.background_bytes += bytes;
            }
            assert_eq!(work.background_lines, expected);
        }
        let mut buffer = TerminalBuffer::new();
        buffer.push(b"stable\npending");
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        set_geometry(&mut state, 0, &[]);
        let text = state.lines[0].styled.clone();
        state.reflow_cursor = 1;
        let old_generation = state.reflow_generation;
        assert!(state.set_wrap_width(Some(px(80.))));
        assert!(!state.exact(0));
        assert_eq!(state.reflow_cursor, 0);
        state.reflow_cursor = 1;
        assert!(state.set_wrap_width(Some(px(40.))));
        assert_eq!(state.reflow_cursor, 0);
        assert_ne!(state.reflow_generation, old_generation);
        assert!(Rc::ptr_eq(&text, &state.lines[0].styled));
        set_geometry(&mut state, 0, &[]);
        let generation = state.reflow_generation;
        state.reflow_cursor = 1;
        assert!(!state.set_wrap_width(Some(px(40.))));
        assert!(state.exact(0));
        assert_eq!(state.reflow_generation, generation);
        assert_eq!(state.reflow_cursor, 1);
    }

    #[test]
    fn released_unicode_selection_does_not_expand_when_output_is_appended() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("prefix  世界 \n\n🦀  ".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        let range = "prefix".len()..state.byte_len;
        state.update_selection(Some(range.clone()), true);
        state.update_selection(Some(range.clone()), false);
        buffer.push(b"appended\nmore");
        state.sync_buffer(&buffer, 1);
        state.update_selection(Some(range.start..state.byte_len), false);
        assert_eq!(state.selected_text().as_deref(), Some("  世界 \n\n🦀  "));
    }

    #[test]
    fn whitespace_and_cross_viewport_reverse_selection_copy_logical_document() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("one  \n\n  two\n世界\n🦀".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        state.update_selection(Some(0..3), false);
        state.clear_selection();
        assert_eq!(state.selected_text(), None);
        state.update_selection(Some(3..9), false);
        assert_eq!(state.selected_text().as_deref(), Some("  \n\n  "));
        state.clear_selection();
        // Neither endpoint has to remain in the painted projection to copy.
        let offscreen = point(px(-1000.), px(-1000.));
        let anchor = state.resolve_offset(
            Some(TextSelectionContentKey::new(state.byte_len as u64)),
            offscreen,
        );
        let cursor = state.resolve_offset(Some(TextSelectionContentKey::new(3)), offscreen);
        state.update_endpoints(anchor, cursor, TextSelectionCoverage::Bounded, false);
        assert!(state.painted.is_empty());
        assert_eq!(
            state.selected_text().as_deref(),
            Some("  \n\n  two\n世界\n🦀")
        );
    }

    #[test]
    fn line_runs_keep_real_newlines_and_global_utf8_offsets() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("界\n\n🦀 \n".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        assert_eq!(
            state
                .lines
                .iter()
                .map(|line| (line.offset, line.text.as_ref()))
                .collect::<Vec<_>>(),
            vec![(0, "界\n"), (4, "\n"), (5, "🦀 \n"), (11, "")]
        );
        let joined: String = state.lines.iter().map(|line| line.text.as_ref()).collect();
        assert_eq!(joined, buffer.text());
    }

    #[test]
    fn appending_reuses_completed_rows_and_updates_only_the_tail() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("first\n界".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        let first = state.lines[0].styled.clone();
        let tail = state.lines[1].styled.clone();
        buffer.push("🦀\nlast".as_bytes());
        state.sync_buffer(&buffer, 1);
        assert!(Rc::ptr_eq(&first, &state.lines[0].styled));
        assert!(!Rc::ptr_eq(&tail, &state.lines[1].styled));
        assert_eq!(state.lines[1].text.as_ref(), "界🦀\n");
        assert_eq!(state.lines[2].offset, "first\n界🦀\n".len());
    }

    #[test]
    fn trimming_reuses_surviving_layouts_but_invalidates_selection() {
        let mut buffer = TerminalBuffer::new();
        for index in 0..MAX_LINES - 1 {
            buffer.push(format!("row {index}\n").as_bytes());
        }
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        let retained = state.lines[1].styled.clone();
        state.update_selection(Some(0..3), false);
        buffer.push(b"next\n");
        state.sync_buffer(&buffer, 1);
        assert!(Rc::ptr_eq(&retained, &state.lines[0].styled));
        assert_eq!(state.lines[0].offset, 0);
        assert_eq!(state.selected_text(), None);
        assert_eq!(state.lines.len(), MAX_LINES);
    }

    #[test]
    fn overwrite_clear_and_generation_reset_invalidate_cached_content() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("界abc".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        state.update_selection(Some(0..3), false);
        buffer.push(b"\rX");
        state.sync_buffer(&buffer, 1);
        assert_eq!(state.selected_text(), None);
        assert_eq!(state.lines[0].text.as_ref(), "Xabc");
        let old = state.lines[0].styled.clone();
        state.sync_buffer(&buffer, 2);
        assert!(!Rc::ptr_eq(&old, &state.lines[0].styled));
        buffer.clear();
        buffer.push(b"fresh");
        state.sync_buffer(&buffer, 2);
        assert_eq!(state.lines[0].text.as_ref(), "fresh");
    }

    #[test]
    fn caret_midpoints_include_right_half_unicode_and_same_row_newlines() {
        use super::nearest_character_boundary;
        // The first glyph is a three-byte scalar; its right half resolves to
        // the following UTF-8 boundary rather than its floor hit-test index.
        assert_eq!(nearest_character_boundary(0, 3, px(4.), px(0.), px(10.)), 0);
        assert_eq!(nearest_character_boundary(0, 3, px(6.), px(0.), px(10.)), 3);
        // A retained separator uses the native half-line-height band at EOL.
        assert_eq!(
            nearest_character_boundary(3, 4, px(14.), px(10.), px(10.)),
            3
        );
        assert_eq!(
            nearest_character_boundary(3, 4, px(16.), px(10.), px(10.)),
            4
        );
    }

    fn render_style() -> RenderStyle {
        RenderStyle {
            text: TextStyle {
                color: rgb(0xd0d0d0).into(),
                ..TextStyle::default()
            },
            rem_size: px(16.),
            scale_factor: 1.,
            background: rgb(0x202020).into(),
        }
    }

    #[test]
    fn styled_unicode_selection_and_runs_use_plain_document_byte_offsets() {
        let mut buffer = TerminalBuffer::new();
        buffer.push("prefix \x1b[31m世界\x1b[0m \n\x1b[1;44m🦀\x1b[0m end".as_bytes());
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        let first = state.lines[0].styled.borrow();
        let runs = text_runs(first.text.len(), &first.styles, &render_style());
        let mut offset = 0;
        let pieces = runs
            .iter()
            .map(|run| {
                let piece = &first.text[offset..offset + run.len];
                offset += run.len;
                piece
            })
            .collect::<Vec<_>>();
        assert_eq!(pieces, ["prefix ", "世界", " \n"]);
        assert_eq!(runs[0].color, render_style().text.color);
        assert_ne!(runs[1].color, runs[0].color);
        assert_eq!(runs[2].color, runs[0].color);
        drop(first);
        let offscreen = point(px(-1000.), px(-1000.));
        let end = "prefix 世界 \n🦀".len();
        let anchor =
            state.resolve_offset(Some(TextSelectionContentKey::new(end as u64)), offscreen);
        let cursor = state.resolve_offset(Some(TextSelectionContentKey::new(7)), offscreen);
        state.update_endpoints(anchor, cursor, TextSelectionCoverage::Bounded, false);
        assert_eq!(state.selected_text().as_deref(), Some("世界 \n🦀"));
        buffer.push(b"\x1b[32m more");
        state.sync_buffer(&buffer, 1);
        assert_eq!(state.selected_text().as_deref(), Some("世界 \n🦀"));
        assert_eq!(state.lines[1].offset, "prefix 世界 \n".len());
    }

    #[test]
    fn recoloring_identical_text_invalidates_only_its_row_and_retains_selection() {
        let mut buffer = TerminalBuffer::new();
        buffer.push(b"stable\n\x1b[31mX");
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        state.update_selection(Some(7..8), false);
        let stable = state.lines[0].styled.clone();
        let old = state.lines[1].styled.clone();
        state.lines[0].width = Some(px(60.));
        state.lines[1].width = Some(px(10.));
        buffer.push(b"\r\x1b[32mX");
        state.sync_buffer(&buffer, 1);
        assert!(Rc::ptr_eq(&stable, &state.lines[0].styled));
        assert!(!Rc::ptr_eq(&old, &state.lines[1].styled));
        assert_eq!(state.lines[0].width, Some(px(60.)));
        assert_eq!(state.lines[1].width, None);
        assert_eq!(state.lines[1].text.as_ref(), "X");
        assert_eq!(
            state.lines[1].styled.borrow().styles[0].style.foreground,
            AnsiColor::Indexed(2)
        );
        assert_eq!(state.selected_text().as_deref(), Some("X"));
    }

    #[test]
    fn font_surface_and_scale_changes_invalidate_measured_rows_but_stable_frames_do_not() {
        let mut buffer = TerminalBuffer::new();
        buffer.push(b"\x1b[31mred\nplain");
        let mut state = TranscriptState::default();
        state.sync_buffer(&buffer, 1);
        let original = render_style();
        state.update_style(original.clone());
        let cached = state.lines[0].styled.clone();
        for line in &mut state.lines {
            line.width = Some(px(42.));
        }
        state.extent_dirty = false;
        state.sync_buffer(&buffer, 1);
        state.update_style(original.clone());
        assert!(!state.extent_dirty);
        assert!(state.lines.iter().all(|line| line.width == Some(px(42.))));
        assert!(Rc::ptr_eq(&cached, &state.lines[0].styled));
        let mut changes = Vec::new();
        let mut changed = original.clone();
        changed.text.font_weight = FontWeight::BOLD;
        changes.push(changed);
        let mut changed = original.clone();
        changed.text.color = rgb(0x222222).into();
        changes.push(changed);
        let mut changed = original.clone();
        changed.background = rgb(0xffffff).into();
        changes.push(changed);
        let mut changed = original.clone();
        changed.rem_size = px(18.);
        changes.push(changed);
        let mut changed = original.clone();
        changed.scale_factor = 1.5;
        changes.push(changed);
        for changed in changes {
            state.update_style(original.clone());
            for line in &mut state.lines {
                line.width = Some(px(42.));
            }
            state.extent_dirty = false;
            state.update_style(changed);
            assert!(state.extent_dirty);
            assert!(state.lines.iter().all(|line| line.width.is_none()));
        }
    }

    #[test]
    fn preferred_keyboard_column_survives_colors_but_not_metric_or_wrap_changes() {
        let mut state = TranscriptState::default();
        let mut style = render_style();
        state.update_style(style.clone());
        state.preferred_x = Some(px(70.));
        style.text.color = rgb(0x222222).into();
        state.update_style(style.clone());
        assert_eq!(state.preferred_x, Some(px(70.)));
        style.background = rgb(0xffffff).into();
        state.update_style(style.clone());
        assert_eq!(state.preferred_x, Some(px(70.)));
        style.rem_size = px(20.);
        state.update_style(style.clone());
        assert_eq!(state.preferred_x, None);
        state.preferred_x = Some(px(80.));
        style.text.font_weight = FontWeight::BOLD;
        state.update_style(style);
        assert_eq!(state.preferred_x, None);
        state.preferred_x = Some(px(90.));
        state.set_wrap_width(Some(px(100.)));
        assert_eq!(state.preferred_x, None);
        state.preferred_x = Some(px(90.));
        state.set_wrap_width(Some(px(100.)));
        assert_eq!(state.preferred_x, Some(px(90.)));
    }

    #[test]
    fn extended_palette_boundaries_and_rgb_are_exact_in_both_themes() {
        for (index, expected) in [
            (16, 0x000000),
            (17, 0x00005f),
            (21, 0x0000ff),
            (22, 0x005f00),
            (51, 0x00ffff),
            (52, 0x5f0000),
            (196, 0xff0000),
            (231, 0xffffff),
            (232, 0x080808),
            (255, 0xeeeeee),
        ] {
            let expected: Hsla = rgb(expected).into();
            assert_eq!(indexed_color(index, false), expected);
            assert_eq!(indexed_color(index, true), expected);
        }
        assert_ne!(indexed_color(0, false), indexed_color(0, true));
        assert_ne!(indexed_color(15, false), indexed_color(15, true));
        let ansi = AnsiStyle {
            foreground: AnsiColor::Rgb(17, 128, 249),
            background: AnsiColor::Rgb(9, 27, 81),
            ..AnsiStyle::default()
        };
        let mut style = render_style();
        for background in [0x000000, 0xffffff] {
            style.background = rgb(background).into();
            let run = styled_run(3, ansi, &style);
            assert_eq!(run.color, Hsla::from(rgb(0x1180f9)));
            assert_eq!(run.background_color, Some(Hsla::from(rgb(0x091b51))));
        }
    }

    #[test]
    fn inverse_resolves_defaults_before_swapping_and_dim_blends_effective_colors() {
        let style = render_style();
        let inverse = styled_run(
            3,
            AnsiStyle {
                inverse: true,
                ..AnsiStyle::default()
            },
            &style,
        );
        assert_eq!(inverse.color, style.background);
        assert_eq!(inverse.background_color, Some(style.text.color));
        let ansi = AnsiStyle {
            foreground: AnsiColor::Rgb(255, 0, 0),
            background: AnsiColor::Rgb(0, 0, 255),
            bold: true,
            dim: true,
            inverse: true,
        };
        let run = styled_run(3, ansi, &style);
        assert_eq!(run.font.weight, FontWeight::BOLD);
        assert_eq!(run.background_color, Some(Hsla::from(rgb(0xff0000))));
        let color: Rgba = run.color.into();
        assert!((color.r - 0.5).abs() < 0.0001);
        assert!(color.g.abs() < 0.0001);
        assert!((color.b - 0.5).abs() < 0.0001);
        let runs = text_runs(
            4,
            &[StyleRun {
                range: 0..3,
                style: ansi,
            }],
            &style,
        );
        assert_eq!(runs[1], style.text.to_run(1));
    }
}
