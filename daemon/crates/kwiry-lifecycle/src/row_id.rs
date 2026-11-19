//! Row identifiers of the §5.8 lifecycle table.
//!
//! The transition function cites these on every step, and the generated
//! table (`crate::table`) is keyed by them, so a row cannot exist in one
//! without the other.

macro_rules! row_ids {
    ($($id:ident),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum RowId { $($id),* }

        impl RowId {
            pub const ALL: &'static [RowId] = &[$(RowId::$id),*];

            pub fn name(self) -> &'static str {
                match self { $(RowId::$id => stringify!($id)),* }
            }

            pub fn index(self) -> usize {
                self as usize
            }
        }
    };
}

row_ids! {
    // M1
    S1, S1L, S2, S3, S4, S5, S6, S7, S8, S8n, S9, S9n, S10, S10n, S11, S12, S13, S14, S15,
    S16, S17, S18,
    // M2
    H1, H2, H3, H4, H5, H6, H7, H8, H9, H10, H11, H12j, H13j, H14, H15, H16, H17, H18, H19, H20, H21,
    H22, H23, H24, H25,
    // M3
    F1, F2, F3, F4, F5, F6, F7, F8, F8r, F8p, F8b, F9, F10, F11,
    // M4a
    A0, A1, A2, A2n, A3, A3n, A4, A5, A6,
    // M4b
    C1, C2, C3, C4, C5, C6, C7, C8, C9, C10, C11, C12, C13, C14, C15, C16, C17, C18, C19, C20,
    C21,
    // M5
    O1, O2, O3, O4, O5, O6, O7, O8, O9, O10, O11, O11g, O12, O13, O14, O14x,
    // M6
    R1, R1r, R2, R2h, R2x, R2d, R2q, R2c, R3, R4, R4a, R5, R6, R6e, R7, R8, R9, R10, R11,
    R11r, R11p, R11b, R12, R13, R14, R15, R16, R17, R18, R19,
    // M7
    L1, L1d, L2, L3, L4, L5, L5f, L6, L7, L8, L9, L10, L11,
    // M8
    T1, T2, T2r, T2h, T2x, T2d, T2q, T3, T4, T5, T6a, T6b, T6r, T6p, T6e, T6c, T6d, T7,
    // M9
    K1, K1i, K2, K2x, K5, K3, K4,
    // M10
    N1, N2, N2c, N3, N4, N5,
    // M11
    P1, P2, P3, P4, P5, P6, P7, P8,
}
