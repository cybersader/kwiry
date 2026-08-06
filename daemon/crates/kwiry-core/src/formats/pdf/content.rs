// SPDX-License-Identifier: MIT OR Apache-2.0

//! Content-stream interpreter.
//!
//! Turns one page's operators into positioned text runs in device space. It
//! deliberately stops there: no space inference, no line assembly, no
//! paragraph or block detection, no reading-order recovery. Those decisions
//! belong to the segmentation step and are not smuggled in here as defaults.
//!
//! A **run** is a maximal contiguous show sequence under an unchanged text
//! state: it breaks at `BT`, at `Tf`, at any repositioning operator, at any
//! change to `Tc/Tw/Tz/Ts/Tr`, at any `q`/`Q` that moves the CTM, and at every
//! numeric element inside a `TJ` array. Breaking on `TJ` adjustments is finer
//! than a segmentation layer needs, and that is the point: within one run the
//! inter-glyph gap is zero by construction, so the only gaps the next step has
//! to reason about are the ones *between* runs, and each of those is
//! recoverable from two origins and an end point.

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object};

use super::error::notice;
use super::fonts::{CodeWidth, FontModel, PageFonts, number};
use super::limits;
use super::split::{self, Window};
use super::state::{GraphicsState, Matrix, TextState, rendering_matrix};
use super::{PdfTextRun, PdfWritingMode};

pub(super) struct PageOutcome {
    pub(super) runs: Vec<PdfTextRun>,
    pub(super) truncated: bool,
    /// A show operator ran under a font whose codes this build cannot decode.
    /// Not a truncation: it declines the whole source, because dropping only
    /// the codes it cannot map would change section content and therefore
    /// chunk boundaries. See `super::cmap`.
    pub(super) undecodable_font: bool,
    pub(super) notices: Vec<(&'static str, String)>,
}

/// Aggregate text budget, shared across pages so a document cannot bypass it by
/// spreading the payload thinly.
pub(super) struct TextBudget {
    pub(super) remaining: usize,
    pub(super) exhausted: bool,
}

impl TextBudget {
    pub(super) fn new() -> Self {
        Self {
            remaining: limits::MAX_EXTRACTED_TEXT_BYTES,
            exhausted: false,
        }
    }
}

struct Interpreter<'a, 'doc> {
    fonts: &'a PageFonts<'doc>,
    page_number: u32,
    text_budget: &'a mut TextBudget,

    state: GraphicsState,
    stack: Vec<GraphicsState>,
    text_matrix: Matrix,
    line_matrix: Matrix,
    in_text_object: bool,
    text_object_index: u32,

    runs: Vec<PdfTextRun>,
    glyphs: usize,
    /// Operations interpreted so far on this page. A field rather than a loop
    /// index because the page is decoded in windows and the budget is a page
    /// budget, not a window budget.
    operations: usize,
    /// A budget ended interpretation; no further window is decoded.
    stopped: bool,
    truncated: bool,
    undecodable_font: bool,
    notices: Vec<(&'static str, String)>,
}

pub(super) fn interpret_page(
    document: &Document,
    page_id: lopdf::ObjectId,
    page_number: u32,
    base: Matrix,
    content: &[u8],
    text_budget: &mut TextBudget,
) -> PageOutcome {
    let fonts = PageFonts::resolve(document, page_id);
    let mut interpreter = Interpreter {
        fonts: &fonts,
        page_number,
        text_budget,
        state: GraphicsState::new(base),
        stack: Vec::new(),
        text_matrix: Matrix::IDENTITY,
        line_matrix: Matrix::IDENTITY,
        in_text_object: false,
        text_object_index: 0,
        runs: Vec::new(),
        glyphs: 0,
        operations: 0,
        stopped: false,
        truncated: false,
        undecodable_font: false,
        notices: Vec::new(),
    };
    if fonts.truncated {
        interpreter.note(
            notice::FONT_LIMIT,
            format!(
                "page declares more than {} font resources; the tail was not resolved",
                limits::MAX_FONTS_PER_PAGE
            ),
        );
        interpreter.truncated = true;
    }
    if fonts.widths_truncated {
        interpreter.note(
            notice::FONT_WIDTH_LIMIT,
            format!(
                "a font declares more than {} width entries; the tail was not retained",
                limits::MAX_FONT_WIDTH_ENTRIES
            ),
        );
        interpreter.truncated = true;
    }

    interpreter.interpret(content, limits::MAX_CONTENT_WINDOW_BYTES);

    PageOutcome {
        runs: interpreter.runs,
        truncated: interpreter.truncated,
        undecodable_font: interpreter.undecodable_font,
        notices: interpreter.notices,
    }
}

impl Interpreter<'_, '_> {
    fn note(&mut self, code: &'static str, message: String) {
        if self.notices.len() < limits::MAX_NOTICES
            && !self.notices.iter().any(|(existing, _)| *existing == code)
        {
            self.notices.push((code, message));
        }
    }

    /// Interpret a page's content stream one bounded window at a time.
    ///
    /// The window is what keeps peak allocation off the decompressed stream
    /// length; see `super::split` for why `Content::decode` alone cannot, and
    /// for the two guards that make a boundary safe.
    fn interpret(&mut self, content: &[u8], budget: usize) {
        let mut start = 0usize;
        while start < content.len() && !self.stopped {
            let window = split::window(content, start, budget);
            let end = window.end();
            // A splitter that made no progress would spin; it cannot, because
            // `operation` consumes at least one byte, but the invariant is
            // cheap to enforce and expensive to debug.
            if end <= start && !matches!(window, Window::Unparsable { .. }) {
                self.unparsable("page content stream made no progress");
                return;
            }
            if let Window::Oversize { .. } = window {
                // Never decoded, so it never allocates. Counted as the one
                // operation it is, and declared rather than dropped in silence.
                self.operations = self.operations.saturating_add(1);
                self.note(
                    notice::OPERANDS_LIMIT,
                    format!(
                        "an operation declares more than {} operands; it was not interpreted",
                        limits::MAX_OPERANDS_PER_OPERATION
                    ),
                );
                self.truncated = true;
                start = end;
                continue;
            }
            match Content::decode_strict(&content[start..end]) {
                Ok(decoded) => self.run(&decoded.operations),
                Err(_) => {
                    // The boundary did not survive a strict re-parse. Fall back
                    // to the lenient decode this module used before windowing,
                    // which is exactly what `lopdf` would have produced for the
                    // same bytes, then stop: `many0` stops here too.
                    if let Ok(decoded) = Content::decode(&content[start..end]) {
                        self.run(&decoded.operations);
                    }
                    self.unparsable("page content stream could not be decoded");
                    return;
                }
            }
            if let Window::Unparsable { .. } = window {
                self.unparsable("page content stream ends in an operation that does not parse");
                return;
            }
            start = end;
        }
    }

    fn unparsable(&mut self, message: &str) {
        self.note(notice::CONTENT_UNPARSABLE, message.to_string());
        self.truncated = true;
        self.stopped = true;
    }

    fn run(&mut self, operations: &[Operation]) {
        for operation in operations {
            if self.operations >= limits::MAX_OPERATIONS_PER_PAGE {
                self.note(
                    notice::OPERATION_LIMIT,
                    format!(
                        "page exceeds {} content-stream operators; the remainder was not interpreted",
                        limits::MAX_OPERATIONS_PER_PAGE
                    ),
                );
                self.truncated = true;
                self.stopped = true;
                return;
            }
            self.operations += 1;
            if self.apply(operation).is_break() {
                self.stopped = true;
                return;
            }
        }
    }

    fn apply(&mut self, operation: &Operation) -> std::ops::ControlFlow<()> {
        let operands = &operation.operands;
        match operation.operator.as_str() {
            "q" => {
                if self.stack.len() >= limits::MAX_GRAPHICS_STACK_DEPTH {
                    self.note(
                        notice::GRAPHICS_STACK_LIMIT,
                        format!(
                            "graphics state stack exceeds depth {}; the push was ignored",
                            limits::MAX_GRAPHICS_STACK_DEPTH
                        ),
                    );
                } else {
                    self.stack.push(self.state.clone());
                }
            }
            "Q" => match self.stack.pop() {
                Some(state) => self.state = state,
                None => self.note(
                    notice::GRAPHICS_STACK_UNDERFLOW,
                    "content stream popped an empty graphics state stack".to_string(),
                ),
            },
            "cm" => {
                if let Some(matrix) = matrix_operand(operands) {
                    let composed = matrix.multiply(self.state.ctm);
                    if composed.is_finite() {
                        self.state.ctm = composed;
                    } else {
                        self.note(
                            notice::NON_INVERTIBLE_MATRIX,
                            "content stream produced a non-finite transformation matrix"
                                .to_string(),
                        );
                    }
                }
            }
            "BT" => {
                if self.in_text_object {
                    self.note(
                        notice::NESTED_TEXT_OBJECT,
                        "BT encountered inside an open text object; treated as a reset".to_string(),
                    );
                }
                self.in_text_object = true;
                self.text_object_index = self.text_object_index.saturating_add(1);
                self.text_matrix = Matrix::IDENTITY;
                self.line_matrix = Matrix::IDENTITY;
            }
            "ET" => self.in_text_object = false,
            "Tf" => {
                if let [name, size] = operands.as_slice() {
                    self.state.text.font_key = name.as_name().ok().map(<[u8]>::to_vec);
                    self.state.text.font_size = number(size).unwrap_or(0.0);
                }
            }
            "Tc" => self.state.text.char_spacing = first_number(operands),
            "Tw" => self.state.text.word_spacing = first_number(operands),
            "Tz" => self.state.text.horizontal_scale = first_number(operands) / 100.0,
            "TL" => self.state.text.leading = first_number(operands),
            "Ts" => self.state.text.rise = first_number(operands),
            "Tr" => {
                self.state.text.render_mode = operands
                    .first()
                    .and_then(|object| object.as_i64().ok())
                    .unwrap_or(0);
            }
            "Td" => {
                let (tx, ty) = pair_operand(operands);
                self.translate_line(tx, ty);
            }
            "TD" => {
                let (tx, ty) = pair_operand(operands);
                self.state.text.leading = -ty;
                self.translate_line(tx, ty);
            }
            "Tm" => {
                if let Some(matrix) = matrix_operand(operands)
                    && matrix.is_finite()
                {
                    self.line_matrix = matrix;
                    self.text_matrix = matrix;
                }
            }
            "T*" => self.next_line(),
            "Tj" => {
                if let Some(Object::String(bytes, _)) = operands.first()
                    && self.show(bytes).is_break()
                {
                    return std::ops::ControlFlow::Break(());
                }
            }
            "'" => {
                self.next_line();
                if let Some(Object::String(bytes, _)) = operands.first()
                    && self.show(bytes).is_break()
                {
                    return std::ops::ControlFlow::Break(());
                }
            }
            "\"" => {
                if let [word_spacing, char_spacing, Object::String(bytes, _)] = operands.as_slice()
                {
                    self.state.text.word_spacing = number(word_spacing).unwrap_or(0.0);
                    self.state.text.char_spacing = number(char_spacing).unwrap_or(0.0);
                    let bytes = bytes.clone();
                    self.next_line();
                    if self.show(&bytes).is_break() {
                        return std::ops::ControlFlow::Break(());
                    }
                }
            }
            "TJ" => {
                if let Some(Object::Array(elements)) = operands.first() {
                    for element in elements {
                        match element {
                            Object::String(bytes, _) => {
                                if self.show(bytes).is_break() {
                                    return std::ops::ControlFlow::Break(());
                                }
                            }
                            other => {
                                if let Some(adjustment) = number(other) {
                                    self.adjust(adjustment);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        std::ops::ControlFlow::Continue(())
    }

    fn translate_line(&mut self, tx: f64, ty: f64) {
        let moved = Matrix::translate(tx, ty).multiply(self.line_matrix);
        if moved.is_finite() {
            self.line_matrix = moved;
            self.text_matrix = moved;
        }
    }

    fn next_line(&mut self) {
        let leading = self.state.text.leading;
        self.translate_line(0.0, -leading);
    }

    /// A `TJ` numeric element: `tx = -(n / 1000) · Tfs · Th`, applied to `Tm`.
    fn adjust(&mut self, adjustment: f64) {
        let text = &self.state.text;
        let tx = -adjustment / 1000.0 * text.font_size * text.horizontal_scale;
        let moved = Matrix::translate(tx, 0.0).multiply(self.text_matrix);
        if moved.is_finite() {
            self.text_matrix = moved;
        }
    }

    fn show(&mut self, bytes: &[u8]) -> std::ops::ControlFlow<()> {
        if self.runs.len() >= limits::MAX_RUNS_PER_PAGE {
            self.note(
                notice::RUN_LIMIT,
                format!(
                    "page exceeds {} text runs; the remainder was not emitted",
                    limits::MAX_RUNS_PER_PAGE
                ),
            );
            self.truncated = true;
            return std::ops::ControlFlow::Break(());
        }

        let font_key = self.state.text.font_key.clone();
        let model = font_key
            .as_deref()
            .and_then(|key| self.fonts.model(key))
            .cloned();
        let Some(model) = model else {
            self.note(
                notice::FONT_UNRESOLVED,
                "show operator ran with no resolvable font resource; the operand was skipped"
                    .to_string(),
            );
            self.truncated = true;
            return std::ops::ControlFlow::Continue(());
        };
        let key = font_key.unwrap_or_default();
        if model.vertical {
            self.note(
                notice::VERTICAL_WRITING,
                "vertical writing mode is not measured; the run carries no advance".to_string(),
            );
            self.truncated = true;
        }
        // A font this build cannot decode is recorded, not silently tolerated.
        // The run itself is still emitted — geometry is what this layer
        // reports, and the run carries no text because no code mapped — while
        // the flag tells the segmentation step to decline the *whole* source.
        // Contributing only the codes that happened to map would change section
        // content, which changes chunk boundaries, which mints chunk identities
        // that collide with the other tier's.
        if self.fonts.is_undecodable(&key, &model) && !self.undecodable_font {
            self.note(
                notice::UNDECODABLE_FONT,
                "a font codes glyph ids this extraction profile cannot resolve to text".to_string(),
            );
            self.undecodable_font = true;
        }

        let origin_matrix = rendering_matrix(&self.state.text, self.text_matrix, self.state.ctm);
        if !origin_matrix.is_finite() {
            self.note(
                notice::NON_INVERTIBLE_MATRIX,
                "text rendering matrix is not finite; the run was skipped".to_string(),
            );
            self.truncated = true;
            return std::ops::ControlFlow::Continue(());
        }
        let origin = origin_matrix.origin();

        let mut text = String::new();
        let mut glyph_count = 0usize;
        let mut widths_exact = true;
        let mut advance_text_space = 0.0f64;

        let mut cursor = 0usize;
        while cursor < bytes.len() {
            let code_bytes =
                &bytes[cursor..bytes.len().min(cursor + model.code_len(bytes[cursor]))];
            cursor += code_bytes.len();
            if self.glyphs >= limits::MAX_GLYPHS_PER_PAGE {
                self.note(
                    notice::GLYPH_LIMIT,
                    format!(
                        "page exceeds {} glyphs; the remainder was not measured",
                        limits::MAX_GLYPHS_PER_PAGE
                    ),
                );
                self.truncated = true;
                break;
            }
            self.glyphs += 1;
            glyph_count += 1;

            let before = text.len();
            self.fonts.decode_code(&key, &model, code_bytes, &mut text);
            let added = text.len() - before;
            if added > self.text_budget.remaining {
                text.truncate(before);
                self.text_budget.exhausted = true;
                self.truncated = true;
                self.note(
                    notice::TEXT_LIMIT,
                    format!(
                        "document exceeds {} bytes of extracted text; the remainder was dropped",
                        limits::MAX_EXTRACTED_TEXT_BYTES
                    ),
                );
                break;
            }
            self.text_budget.remaining -= added;

            let code = code_value(code_bytes);
            let (width, exact) = model.glyph_width(code);
            widths_exact &= exact;
            advance_text_space += glyph_advance(&self.state.text, &model, width, code_bytes);
        }

        let advance_matrix = self.text_matrix.multiply(self.state.ctm);
        let advance = advance_matrix.apply_vector(advance_text_space, 0.0);
        // Direction, not displacement: taken from the matrix so it survives a
        // font that declared no widths. See `PdfTextRun::advance_direction`.
        let direction = advance_matrix.apply_vector(1.0, 0.0);
        let direction = if direction.iter().all(|value| value.is_finite()) {
            direction
        } else {
            [0.0, 0.0]
        };
        let end = [origin[0] + advance[0], origin[1] + advance[1]];
        let font_size = self.state.text.font_size * advance_matrix.scale_magnitude();

        self.runs.push(PdfTextRun {
            page_number: self.page_number,
            text_object_index: self.text_object_index,
            run_index: u32::try_from(self.runs.len()).unwrap_or(u32::MAX),
            text,
            origin,
            end,
            advance_direction: direction,
            font_size,
            font_resource: String::from_utf8_lossy(&key).into_owned(),
            char_spacing: self.state.text.char_spacing,
            word_spacing: self.state.text.word_spacing,
            horizontal_scale: self.state.text.horizontal_scale,
            rise: self.state.text.rise,
            render_mode: self.state.text.render_mode,
            glyph_count,
            writing_mode: if model.vertical {
                PdfWritingMode::Vertical
            } else {
                PdfWritingMode::Horizontal
            },
            geometry_exact: widths_exact && !model.vertical && !model.type3,
        });

        if advance_text_space.is_finite() {
            let moved = Matrix::translate(advance_text_space, 0.0).multiply(self.text_matrix);
            if moved.is_finite() {
                self.text_matrix = moved;
            }
        }

        if self.text_budget.exhausted {
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    }
}

/// `tx = ((w0 − Tj/1000)·Tfs + Tc + Tw) · Th` (PDF 1.7 §9.4.4), with the `Tj`
/// term applied separately by [`Interpreter::adjust`]. `Tw` applies only to the
/// single-byte code 32, never to a two-byte CID that happens to equal 32.
fn glyph_advance(text: &TextState, model: &FontModel, width: f64, code_bytes: &[u8]) -> f64 {
    let word_spacing = if model.code_width == CodeWidth::One && code_bytes == [0x20] {
        text.word_spacing
    } else {
        0.0
    };
    (width * text.font_size + text.char_spacing + word_spacing) * text.horizontal_scale
}

fn code_value(code_bytes: &[u8]) -> u32 {
    code_bytes.iter().fold(0u32, |accumulator, byte| {
        (accumulator << 8) | u32::from(*byte)
    })
}

fn first_number(operands: &[Object]) -> f64 {
    operands.first().and_then(number).unwrap_or(0.0)
}

fn pair_operand(operands: &[Object]) -> (f64, f64) {
    match operands {
        [tx, ty] => (number(tx).unwrap_or(0.0), number(ty).unwrap_or(0.0)),
        _ => (0.0, 0.0),
    }
}

fn matrix_operand(operands: &[Object]) -> Option<Matrix> {
    let [a, b, c, d, e, f] = operands else {
        return None;
    };
    Some(Matrix([
        number(a)?,
        number(b)?,
        number(c)?,
        number(d)?,
        number(e)?,
        number(f)?,
    ]))
}
