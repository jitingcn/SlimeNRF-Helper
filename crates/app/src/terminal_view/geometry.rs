use std::{ops::Range, sync::Arc};

use gpui_kit::{Pixels, Point, WrappedLineLayout, point, px};

use super::nearest_character_boundary;

#[derive(Clone, Copy, PartialEq)]
pub(super) struct LayoutKey {
    pub style_generation: u64,
    pub wrap_width: Option<Pixels>,
    pub row_height: Pixels,
}

impl Default for LayoutKey {
    fn default() -> Self {
        Self {
            style_generation: 0,
            wrap_width: None,
            // Initial model geometry precedes a window; render replaces this
            // with 1.25 rem before any text is measured or presented.
            row_height: px(20.),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum RowAffinity {
    Upstream,
    #[default]
    Downstream,
}

pub(super) struct VisualRow {
    pub bytes: Range<usize>,
    pub unwrapped_start_x: Pixels,
    pub unwrapped_end_x: Pixels,
}

pub(super) struct LineGeometry {
    pub key: LayoutKey,
    pub content_layout: Arc<WrappedLineLayout>,
    pub rows: Vec<VisualRow>,
}

impl LineGeometry {
    pub fn new(key: LayoutKey, content_layout: Arc<WrappedLineLayout>) -> Self {
        let mut geometry = Self {
            key,
            content_layout,
            rows: Vec::new(),
        };
        geometry.rebuild_rows();
        geometry
    }

    pub fn reflow(&mut self, key: LayoutKey, content_layout: Arc<WrappedLineLayout>) {
        self.key = key;
        self.content_layout = content_layout;
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
        let unwrapped = &self.content_layout.unwrapped_layout;
        self.rows.clear();
        self.rows
            .reserve(self.content_layout.wrap_boundaries.len() + 1);
        let mut start = 0;
        let mut start_x = px(0.);
        for boundary in &self.content_layout.wrap_boundaries {
            let glyph = &unwrapped.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
            self.rows.push(VisualRow {
                bytes: start..glyph.index,
                unwrapped_start_x: start_x,
                unwrapped_end_x: glyph.position.x,
            });
            start = glyph.index;
            start_x = glyph.position.x;
        }
        self.rows.push(VisualRow {
            bytes: start..unwrapped.len,
            unwrapped_start_x: start_x,
            unwrapped_end_x: unwrapped.width,
        });
    }

    pub fn row_for_byte(&self, byte: usize, affinity: RowAffinity) -> usize {
        row_for_byte(&self.rows, byte, affinity)
    }

    pub fn position(&self, byte: usize, affinity: RowAffinity) -> Point<Pixels> {
        let row_index = self.row_for_byte(byte, affinity);
        let row = &self.rows[row_index];
        point(
            self.content_layout.unwrapped_layout.x_for_index(byte) - row.unwrapped_start_x,
            self.key.row_height * row_index as f32,
        )
    }

    pub fn byte_at_point(&self, text: &str, position: Point<Pixels>) -> usize {
        let row_index = ((position.y.max(px(0.)) / self.key.row_height).floor() as usize)
            .min(self.rows.len() - 1);
        let row = &self.rows[row_index];
        let x = position.x + row.unwrapped_start_x;
        let unwrapped = &self.content_layout.unwrapped_layout;
        if x < row.unwrapped_start_x {
            return row.bytes.start;
        }
        if x >= row.unwrapped_end_x {
            return if row_index + 1 == self.rows.len() && text.ends_with('\n') {
                nearest_character_boundary(
                    row.bytes.end,
                    text.len(),
                    x,
                    row.unwrapped_end_x,
                    self.key.row_height / 2.,
                )
            } else {
                row.bytes.end
            };
        }
        let index = unwrapped
            .index_for_x(x)
            .unwrap_or(row.bytes.end)
            .clamp(row.bytes.start, row.bytes.end);
        let Some(character) = text[index..row.bytes.end].chars().next() else {
            return index;
        };
        let next = index + character.len_utf8();
        let start_x = unwrapped.x_for_index(index);
        let end_x = unwrapped.x_for_index(next).min(row.unwrapped_end_x);
        nearest_character_boundary(index, next, x, start_x, end_x - start_x)
    }

    pub fn selection_rows(
        &self,
        selection: Range<usize>,
        real_newline: bool,
        document_width: Pixels,
    ) -> impl Iterator<Item = (usize, Range<Pixels>)> + '_ {
        let content_len = self.content_layout.len();
        let first = self.row_for_byte(selection.start.min(content_len), RowAffinity::Downstream);
        let last = if real_newline && selection.end > content_len {
            self.rows.len() - 1
        } else {
            self.row_for_byte(selection.end.min(content_len), RowAffinity::Upstream)
        };
        let unwrapped = &self.content_layout.unwrapped_layout;
        let start_x = unwrapped.x_for_index(selection.start.min(content_len));
        let end_x = unwrapped.x_for_index(selection.end.min(content_len));
        self.rows
            .iter()
            .enumerate()
            .skip(first)
            .take((last + 1).saturating_sub(first))
            .filter_map(move |(index, row)| {
                let start = selection.start.max(row.bytes.start);
                let end = selection.end.min(row.bytes.end);
                let newline = real_newline
                    && index + 1 == self.rows.len()
                    && selection.end > row.bytes.end
                    && selection.start <= row.bytes.end;
                if start >= end && !newline {
                    return None;
                }
                let left = if selection.start <= row.bytes.start {
                    px(0.)
                } else {
                    start_x - row.unwrapped_start_x
                };
                let right = if newline {
                    document_width
                } else if selection.end >= row.bytes.end {
                    row.unwrapped_end_x - row.unwrapped_start_x
                } else {
                    end_x - row.unwrapped_start_x
                };
                Some((index, left..right))
            })
    }
}

pub(super) fn row_for_byte(rows: &[VisualRow], byte: usize, affinity: RowAffinity) -> usize {
    rows.partition_point(|row| match affinity {
        RowAffinity::Upstream => row.bytes.end < byte,
        RowAffinity::Downstream => row.bytes.end <= byte,
    })
    .min(rows.len() - 1)
}

pub(super) fn line_at_y(prefix_rows: &[u32], y: Pixels, row_height: Pixels) -> usize {
    let row = (y.max(px(0.)) / row_height).floor() as u32;
    prefix_rows
        .partition_point(|prefix| *prefix <= row)
        .saturating_sub(1)
        .min(prefix_rows.len().saturating_sub(2))
}

pub(super) fn visible_lines(
    prefix_rows: &[u32],
    top: Pixels,
    bottom: Pixels,
    row_height: Pixels,
) -> Range<usize> {
    let count = prefix_rows.len().saturating_sub(1);
    let height = row_height * prefix_rows.last().copied().unwrap_or(0) as f32;
    if count == 0 || top >= height || bottom <= px(0.) {
        return count..count;
    }
    let start = line_at_y(prefix_rows, top, row_height);
    let end = prefix_rows.partition_point(|prefix| row_height * (*prefix as f32) < bottom);
    start..end.min(count).max(start)
}

#[derive(Clone, Copy)]
pub(super) struct ViewportAnchor {
    pub generation: u64,
    pub line_id: u64,
    pub byte: usize,
    pub affinity: RowAffinity,
    pub intra_row_y: Pixels,
}

#[cfg(test)]
pub(super) fn geometry_fixture(text: &str, seams: &[usize]) -> LineGeometry {
    use gpui_kit::{FontId, GlyphId, LineLayout, ShapedGlyph, ShapedRun, WrapBoundary};

    let content = text.strip_suffix('\n').unwrap_or(text);
    let glyphs = content
        .char_indices()
        .enumerate()
        .map(|(index, (byte, _))| ShapedGlyph {
            id: GlyphId(index as u32),
            position: point(px(index as f32 * 10.), px(0.)),
            index: byte,
            is_emoji: false,
        })
        .collect::<Vec<_>>();
    let wrap_boundaries = seams
        .iter()
        .map(|byte| WrapBoundary {
            run_ix: 0,
            glyph_ix: glyphs
                .iter()
                .position(|glyph| glyph.index == *byte)
                .unwrap(),
        })
        .collect();
    let width = px(glyphs.len() as f32 * 10.);
    LineGeometry::new(
        LayoutKey::default(),
        Arc::new(WrappedLineLayout {
            unwrapped_layout: Arc::new(LineLayout {
                width,
                len: content.len(),
                runs: vec![ShapedRun {
                    font_id: FontId(0),
                    glyphs,
                }],
                ..Default::default()
            }),
            wrap_boundaries,
            wrap_width: Some(px(30.)),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        LayoutKey, LineGeometry, RowAffinity, VisualRow, geometry_fixture, line_at_y, row_for_byte,
        visible_lines,
    };
    use gpui_kit::{LineLayout, WrappedLineLayout, point, px};
    use std::sync::Arc;

    fn rows() -> Vec<VisualRow> {
        vec![
            VisualRow {
                bytes: 0..4,
                unwrapped_start_x: px(0.),
                unwrapped_end_x: px(20.),
            },
            VisualRow {
                bytes: 4..11,
                unwrapped_start_x: px(20.),
                unwrapped_end_x: px(50.),
            },
            VisualRow {
                bytes: 11..15,
                unwrapped_start_x: px(50.),
                unwrapped_end_x: px(70.),
            },
        ]
    }

    #[gpui_kit::test]
    fn native_wrapping_retains_utf8_ansi_content_and_ignores_the_trailing_empty_layout(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use super::super::{RenderStyle, text_runs};
        use crate::terminal::TerminalBuffer;
        use gpui_kit::{SharedString, TextStyle, rgb};

        cx.update(|cx| {
            let mut buffer = TerminalBuffer::new();
            let content = "abcdefghijklmno世界🦀".repeat(24);
            buffer.push(format!("\x1b[1;44m{content}\x1b[0m\n").as_bytes());
            let (_, _, text, spans, newline) = buffer.rows().next().unwrap();
            assert!(newline);
            let text: SharedString = format!("{text}\n").into();
            let style = RenderStyle {
                text: TextStyle::default(),
                rem_size: px(16.),
                scale_factor: 1.,
                background: rgb(0x202020).into(),
            };
            let runs = text_runs(text.len(), spans, &style);
            assert!(runs.iter().any(|run| run.background_color.is_some()));
            let text_system = gpui_kit::WindowTextSystem::new(cx.text_system().clone());
            let shaped = text_system
                .shape_text(text.clone(), px(12.), &runs, Some(px(84.)), None)
                .unwrap();
            assert_eq!(shaped.len(), 2);
            assert_eq!(shaped.last().unwrap().len(), 0);
            let first = &shaped[0];
            let geometry = LineGeometry::new(
                LayoutKey {
                    style_generation: 1,
                    wrap_width: Some(px(84.)),
                    ..LayoutKey::default()
                },
                Arc::new(WrappedLineLayout {
                    unwrapped_layout: first.unwrapped_layout.clone(),
                    wrap_boundaries: first.wrap_boundaries.clone(),
                    wrap_width: first.wrap_width,
                }),
            );
            assert!(geometry.rows.len() > 2);
            assert_eq!(geometry.rows.last().unwrap().bytes.end, text.len() - 1);
            for row in &geometry.rows {
                assert!(text.is_char_boundary(row.bytes.start));
                assert!(text.is_char_boundary(row.bytes.end));
            }
            let (row_index, row, next, width) = geometry
                .rows
                .iter()
                .enumerate()
                .skip(1)
                .find_map(|(index, row)| {
                    let character = text[row.bytes.clone()].chars().next()?;
                    let next = row.bytes.start + character.len_utf8();
                    let width = geometry.content_layout.unwrapped_layout.x_for_index(next)
                        - row.unwrapped_start_x;
                    (width > px(0.)).then_some((index, row, next, width))
                })
                .unwrap();
            let y = geometry.key.row_height * row_index as f32 + px(1.);
            assert_eq!(
                geometry.byte_at_point(&text, point(width / 4., y)),
                row.bytes.start
            );
            assert_eq!(geometry.byte_at_point(&text, point(width * 0.75, y)), next);
            assert_eq!(
                geometry
                    .position(row.bytes.start, RowAffinity::Downstream)
                    .x,
                px(0.)
            );
        });
    }

    #[test]
    fn utf8_soft_seams_have_distinct_upstream_and_downstream_rows() {
        let text = "a界世界 🦀";
        let rows = rows();
        for row in &rows {
            assert!(text.is_char_boundary(row.bytes.start));
            assert!(text.is_char_boundary(row.bytes.end));
        }
        assert_eq!(row_for_byte(&rows, 4, RowAffinity::Upstream), 0);
        assert_eq!(row_for_byte(&rows, 4, RowAffinity::Downstream), 1);
        assert_eq!(row_for_byte(&rows, 11, RowAffinity::Upstream), 1);
        assert_eq!(row_for_byte(&rows, 11, RowAffinity::Downstream), 2);
        assert_eq!(row_for_byte(&rows, 15, RowAffinity::Downstream), 2);
    }

    #[test]
    fn prefix_heights_locate_partial_continuations_and_real_empty_lines() {
        let prefix = [0, 3, 4, 5, 9];
        assert_eq!(visible_lines(&prefix, px(21.), px(81.), px(20.)), 0..3);
        assert_eq!(visible_lines(&prefix, px(60.), px(80.), px(20.)), 1..2);
        assert_eq!(visible_lines(&prefix, px(-20.), px(20.), px(20.)), 0..1);
        assert_eq!(visible_lines(&prefix, px(180.), px(200.), px(20.)), 4..4);
        assert_eq!(line_at_y(&prefix, px(99.), px(20.)), 2);
        assert_eq!(line_at_y(&prefix, px(100.), px(20.)), 3);
    }

    #[test]
    fn empty_content_has_one_visual_row_and_only_a_real_newline_extends_selection() {
        let geometry = LineGeometry::new(
            LayoutKey::default(),
            Arc::new(WrappedLineLayout {
                unwrapped_layout: Arc::new(LineLayout::default()),
                ..Default::default()
            }),
        );
        assert_eq!(geometry.rows.len(), 1);
        assert!(
            geometry
                .selection_rows(0..1, false, px(100.))
                .next()
                .is_none()
        );
        assert_eq!(
            geometry
                .selection_rows(0..1, true, px(100.))
                .collect::<Vec<_>>(),
            vec![(0, px(0.)..px(100.))]
        );
    }

    #[test]
    fn continuation_midpoints_resolve_utf8_bytes_without_upstream_seam_coordinates() {
        let text = "a界世界 🦀\n";
        let geometry = geometry_fixture(text, &[4, 11]);
        assert_eq!(
            geometry.position(4, RowAffinity::Upstream),
            point(px(20.), px(0.))
        );
        assert_eq!(
            geometry.position(4, RowAffinity::Downstream),
            point(px(0.), px(20.))
        );
        for (position, expected) in [
            (point(px(-2.), px(21.)), 4),
            (point(px(4.), px(21.)), 4),
            (point(px(6.), px(21.)), 7),
            (point(px(29.), px(21.)), 11),
            (point(px(4.), px(41.)), 11),
            (point(px(6.), px(41.)), 15),
            (point(px(14.), px(41.)), 15),
            (point(px(16.), px(41.)), 16),
        ] {
            let byte = geometry.byte_at_point(text, position);
            assert_eq!(byte, expected);
            assert!(text.is_char_boundary(byte));
        }
    }

    #[test]
    fn selection_crosses_soft_rows_but_extends_only_the_selected_real_newline() {
        let geometry = geometry_fixture("a界世界 🦀\n", &[4, 11]);
        assert_eq!(
            geometry
                .selection_rows(1..15, true, px(100.))
                .collect::<Vec<_>>(),
            vec![
                (0, px(10.)..px(20.)),
                (1, px(0.)..px(30.)),
                (2, px(0.)..px(10.))
            ],
        );
        assert_eq!(
            geometry
                .selection_rows(1..16, true, px(100.))
                .collect::<Vec<_>>(),
            vec![
                (0, px(10.)..px(20.)),
                (1, px(0.)..px(30.)),
                (2, px(0.)..px(100.))
            ],
        );
        assert_eq!(
            geometry
                .selection_rows(4..11, false, px(100.))
                .collect::<Vec<_>>(),
            vec![(1, px(0.)..px(30.))],
        );
    }
}
