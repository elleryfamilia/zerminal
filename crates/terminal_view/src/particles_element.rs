// Custom GPUI element that paints the ambient particle screensaver behind
// the terminal grid. Constructed each frame from `TerminalView::render` only
// when the screensaver is active, mounted as the EARLIER sibling of
// `TerminalElement` inside the `terminal-view-container` div so it paints
// below the terminal text. The terminal's full-bounds bg quad is suppressed
// while the screensaver is active (see `TerminalElement::screensaver_active`).
//
// Per-row rendering uses the same `text_system().shape_line(...).paint(...)`
// API the terminal grid uses (`terminal_element.rs::BatchedTextRun::paint`)
// so font/cell-width/line-height alignment is exact. Empty cells use a
// transparent black `TextRun` of length-1 byte (a space character) to keep
// the run length without painting visible glyphs.

use std::cell::RefCell;

use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Point, TextAlign, TextRun, TextStyle, WeakEntity, Window, px, relative,
    transparent_black,
};
use util::ResultExt;

use crate::TerminalView;
use crate::particles::Particles;
use crate::terminal_element::terminal_text_style_and_metrics;

pub struct ParticlesElement {
    view: WeakEntity<TerminalView>,
    // Reusable per-row scratch buffers, allocated once and `clear`ed each row.
    scratch: RefCell<RowScratch>,
}

#[derive(Default)]
struct RowScratch {
    text: String,
    runs: Vec<TextRun>,
}

pub struct ParticlesPrepaint {
    text_style: TextStyle,
    cell_width: Pixels,
    line_height_px: f32,
    grid_origin: Point<Pixels>,
    cols: usize,
    rows: usize,
    // Snapshot of `(row, col, glyph, color)` tuples grouped by row, only for
    // rows the simulation reports as dirty. Empty rows are absent entirely.
    dirty_row_indices: Vec<usize>,
    cells_by_row: Vec<Vec<(usize, char, gpui::Hsla)>>,
}

impl ParticlesElement {
    pub fn new(view: WeakEntity<TerminalView>) -> Self {
        ParticlesElement {
            view,
            scratch: RefCell::new(RowScratch::default()),
        }
    }
}

impl IntoElement for ParticlesElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ParticlesElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<ParticlesPrepaint>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = gpui::Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        style.position = gpui::Position::Absolute;
        style.inset.top = px(0.0).into();
        style.inset.left = px(0.0).into();
        let layout_id = window.request_layout(style, None, cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let view_entity = self.view.upgrade()?;
        let mode = view_entity.read(cx).mode().clone();
        let (text_style, dimensions, gutter, line_height_px, _bg) =
            terminal_text_style_and_metrics(bounds, &mode, window, cx);

        let cell_width = dimensions.cell_width;
        let grid_origin = bounds.origin + Point::new(gutter, px(0.0));

        // Snapshot the simulation state. `read` is a plain field access;
        // `Particles::cell` is pure.
        let view_ref = view_entity.read(cx);
        let particles: &Particles = view_ref.screensaver()?;
        let cols = particles.cols;
        let rows = particles.rows;
        let dirty = particles.dirty_rows();
        let mut dirty_row_indices = Vec::with_capacity(rows);
        let mut cells_by_row = Vec::with_capacity(rows);
        for row in 0..rows {
            if !dirty.get(row).copied().unwrap_or(false) {
                continue;
            }
            let mut cells = Vec::new();
            for col in 0..cols {
                if let Some((glyph, color)) = particles.cell(col, row) {
                    cells.push((col, glyph, color));
                }
            }
            if !cells.is_empty() {
                dirty_row_indices.push(row);
                cells_by_row.push(cells);
            }
        }

        Some(ParticlesPrepaint {
            text_style,
            cell_width,
            line_height_px,
            grid_origin,
            cols,
            rows,
            dirty_row_indices,
            cells_by_row,
        })
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(prepaint) = prepaint else {
            return;
        };

        if prepaint.cols == 0 || prepaint.rows == 0 {
            return;
        }

        let font = prepaint.text_style.font();
        let font_size = prepaint
            .text_style
            .font_size
            .to_pixels(window.rem_size());
        let line_height = px(prepaint.line_height_px);
        let cell_width = prepaint.cell_width;

        window.paint_layer(bounds, |window| {
            let mut scratch = self.scratch.borrow_mut();

            for (row_idx, cells) in prepaint
                .dirty_row_indices
                .iter()
                .copied()
                .zip(prepaint.cells_by_row.iter())
            {
                scratch.text.clear();
                scratch.runs.clear();

                let mut next_col = 0usize;
                for (col, glyph, color) in cells.iter().copied() {
                    if col < next_col {
                        // Defensive: Particles::cell always returns sorted-by-col
                        // tuples per row, but skip out-of-order entries instead
                        // of risking a panic if that invariant ever drifts.
                        continue;
                    }
                    if col > next_col {
                        let blank_count = col - next_col;
                        for _ in 0..blank_count {
                            scratch.text.push(' ');
                        }
                        scratch.runs.push(TextRun {
                            len: blank_count,
                            font: font.clone(),
                            color: transparent_black(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        });
                    }

                    let glyph_len = glyph.len_utf8();
                    scratch.text.push(glyph);
                    scratch.runs.push(TextRun {
                        len: glyph_len,
                        font: font.clone(),
                        color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    });

                    next_col = col + 1;
                }

                if scratch.text.is_empty() {
                    continue;
                }

                let row_origin = prepaint.grid_origin
                    + Point::new(px(0.0), line_height * row_idx as f32);
                let shaped = window.text_system().shape_line(
                    scratch.text.clone().into(),
                    font_size,
                    &scratch.runs,
                    Some(cell_width),
                );
                shaped
                    .paint(row_origin, line_height, TextAlign::Left, None, window, cx)
                    .log_err();
            }
        });
    }
}

// Helpers reaching into `TerminalView` for the prepaint snapshot are
// implemented as inherent methods on `TerminalView` (in terminal_view.rs):
// `mode()`, `screensaver()`, `screensaver_mut()`, `is_visible_for_screensaver_cached()`.
// Used here only via the `view` weak handle.
