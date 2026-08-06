// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-owned transform and interpreter state.
//!
//! `lopdf` tracks no matrix at all — its own `extract_text_chunks_from_page`
//! reacts to seven operators (`Tf Tj TJ ' " T* ET`) and drops the rest. Every
//! coordinate this module produces is computed here, from `lopdf::Object`
//! operands, and no `lopdf` type escapes the `pdf` module.
//!
//! Convention is PDF's own: row vectors, `[x y 1] × M`, with
//! `M = [a b 0; c d 0; e f 1]` stored as `[a, b, c, d, e, f]`.

/// A PDF transformation matrix in `[a, b, c, d, e, f]` order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Matrix(pub(super) [f64; 6]);

impl Matrix {
    pub(super) const IDENTITY: Self = Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub(super) const fn translate(tx: f64, ty: f64) -> Self {
        Self([1.0, 0.0, 0.0, 1.0, tx, ty])
    }

    /// `self × other`, with `self` applied first.
    pub(super) fn multiply(self, other: Self) -> Self {
        let [a1, b1, c1, d1, e1, f1] = self.0;
        let [a2, b2, c2, d2, e2, f2] = other.0;
        Self([
            a1 * a2 + b1 * c2,
            a1 * b2 + b1 * d2,
            c1 * a2 + d1 * c2,
            c1 * b2 + d1 * d2,
            e1 * a2 + f1 * c2 + e2,
            e1 * b2 + f1 * d2 + f2,
        ])
    }

    /// Translation component — the image of the origin, i.e. row 3.
    pub(super) fn origin(self) -> [f64; 2] {
        [self.0[4], self.0[5]]
    }

    /// Image of a direction vector: the linear part only, no translation.
    pub(super) fn apply_vector(self, x: f64, y: f64) -> [f64; 2] {
        let [a, b, c, d, ..] = self.0;
        [x * a + y * c, x * b + y * d]
    }

    /// `sqrt(|det|)`. Used as the isotropic scale factor for reporting an
    /// effective font size; for the anisotropic case it is the geometric mean,
    /// which is the honest single number.
    pub(super) fn scale_magnitude(self) -> f64 {
        let [a, b, c, d, ..] = self.0;
        (a * d - b * c).abs().sqrt()
    }

    pub(super) fn is_finite(self) -> bool {
        self.0.iter().all(|value| value.is_finite())
    }
}

/// Text state. Per PDF 1.7 §9.3 these parameters live in the **graphics**
/// state, so they survive `BT`/`ET` and are saved and restored by `q`/`Q`.
/// `Tm`/`Tlm` deliberately are not here: they are text-object state, reset by
/// `BT` and untouched by `q`/`Q`.
#[derive(Debug, Clone)]
pub(super) struct TextState {
    pub(super) font_key: Option<Vec<u8>>,
    pub(super) font_size: f64,
    pub(super) char_spacing: f64,
    pub(super) word_spacing: f64,
    /// `Tz / 100`.
    pub(super) horizontal_scale: f64,
    pub(super) leading: f64,
    pub(super) rise: f64,
    pub(super) render_mode: i64,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            font_key: None,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct GraphicsState {
    pub(super) ctm: Matrix,
    pub(super) text: TextState,
}

impl GraphicsState {
    pub(super) fn new(ctm: Matrix) -> Self {
        Self {
            ctm,
            text: TextState::default(),
        }
    }
}

/// `Trm = [Tfs·Th 0 0; 0 Tfs 0; 0 Ts 1] × Tm × CTM` (PDF 1.7 §9.4.4).
pub(super) fn rendering_matrix(text: &TextState, text_matrix: Matrix, ctm: Matrix) -> Matrix {
    let parameters = Matrix([
        text.font_size * text.horizontal_scale,
        0.0,
        0.0,
        text.font_size,
        0.0,
        text.rise,
    ]);
    parameters.multiply(text_matrix).multiply(ctm)
}
