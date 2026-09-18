//! SVQ3 macroblock types, the intra 4×4 prediction-mode elements and
//! the motion-precision selector.
//!
//! * **Macroblock type code numbers** —
//!   `docs/video/svq3/spec/07-slice-and-macroblock-layer.md` §5: the
//!   meaning of `uvlc mb_type` depends on the slice type, each loop
//!   subtracting its own base before indexing the 24-record intra
//!   16×16 parameter array of `tables/03`:
//!
//!   | Slice | code numbers | kind |
//!   | --- | --- | --- |
//!   | I | 0 | intra 4×4 with explicit prediction modes |
//!   | I | 1 … 24 | intra 16×16, record `c − 1` |
//!   | P | 0 … 7 | inter (0 = skip) — `spec/08` |
//!   | P | 8 | intra 4×4 |
//!   | P | 9 … 32 | intra 16×16, record `c − 9` |
//!   | P | 33 | flat-128 intra |
//!   | B | 0 … 3 | inter (not specified beyond the dispatch) |
//!   | B | 4 | intra 4×4 |
//!   | B | 5 … 28 | intra 16×16, record `c − 5` |
//!
//!   Record `r` factors as `(intra16x16_pred_mode, cbp_chroma, luma_ac)
//!   = (r mod 4, ⌊r/4⌋ mod 3, ⌊r/12⌋)` (spec/04 §4.5).
//! * **Intra 4×4 prediction modes** — spec/07 §6 item 1 and §10.2 with
//!   `tables/07` and `tables/08` ([`crate::svq3_tables`]): eight pair
//!   codes, each resolved into two *ranks* for blocks `2i` and
//!   `2i + 1` in quadrant-major block order; a rank becomes a mode
//!   through the neighbour context `(top, left)`, where a context
//!   value is 0 for an unavailable neighbour block and `mode + 1` for
//!   a decoded intra 4×4 block, every other decoded block (intra
//!   16×16, flat-128, inter) contributing 1. Modes: 0 = DC,
//!   1 = vertical, 2 = horizontal, 3 = diagonal-down-right, 4 = the
//!   averaged diagonal (`spec/01` Gap 3).
//! * **Precision selector** — spec/08 §3 / spec/05 §2.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::svq3::{read_universal_code, Svq3FrameType};
use crate::svq3_tables::{
    SVQ3_INTRA4X4_PRED_MODE_CONTEXT, SVQ3_INTRA4X4_PRED_MODE_ILLEGAL, SVQ3_INTRA4X4_PRED_MODE_PAIRS,
};

/// Largest I-slice macroblock type code number (spec/07 §5).
pub const I_SLICE_MB_TYPE_MAX: u32 = 24;
/// Largest P-slice macroblock type code number.
pub const P_SLICE_MB_TYPE_MAX: u32 = 33;
/// Largest B-slice macroblock type code number.
pub const B_SLICE_MB_TYPE_MAX: u32 = 28;
/// P-slice code number of the explicit-mode intra 4×4 macroblock.
pub const P_SLICE_INTRA4X4_CODE: u32 = 8;
/// P-slice code number of the flat-128 intra macroblock.
pub const P_SLICE_FLAT128_CODE: u32 = 33;
/// B-slice code number of the explicit-mode intra 4×4 macroblock.
pub const B_SLICE_INTRA4X4_CODE: u32 = 4;

/// The parameter triple an intra 16×16 macroblock type record carries
/// (`tables/03`, spec/04 §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intra16x16Params {
    /// `intra16x16_pred_mode` 0…3: 0 = DC, 1 = vertical,
    /// 2 = horizontal, 3 = plane (spec/07 §10.1).
    pub pred_mode: u8,
    /// The chroma class of spec/03 §1.2 (0 / 1 / 2).
    pub cbp_chroma: u8,
    /// Whether the sixteen luma AC lists are present (spec/04 §4.3).
    pub luma_ac: bool,
}

impl Intra16x16Params {
    /// Factor record index `r` (0…23) into its triple.
    #[must_use]
    pub const fn from_record(r: u32) -> Option<Self> {
        if r > 23 {
            return None;
        }
        Some(Self {
            pred_mode: (r % 4) as u8,
            cbp_chroma: ((r / 4) % 3) as u8,
            luma_ac: r / 12 == 1,
        })
    }

    /// The inverse of [`Self::from_record`].
    #[must_use]
    pub const fn record(self) -> u32 {
        self.pred_mode as u32 + 4 * self.cbp_chroma as u32 + 12 * self.luma_ac as u32
    }
}

/// The three intra macroblock kinds (spec/07 §5–§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntraMbKind {
    /// Intra 4×4 with explicit prediction modes (spec/07 §6).
    Intra4x4,
    /// Intra 16×16 with the implied pattern of its record (spec/07 §7).
    Intra16x16(Intra16x16Params),
    /// The P-slice flat-128 intra macroblock: luma and chroma predicted
    /// as the constant 128, an explicit intra-table pattern and the
    /// §6 item 3–6 body with no prediction-mode elements.
    Flat128,
}

/// P-slice inter macroblock partitionings (spec/08 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PFrameInterMode {
    /// Code 0 — skip: zero-motion copy, no further elements (§5).
    Skip,
    /// Code 1 — one 16×16 partition.
    Inter16x16,
    /// Code 2 — two 8×16 partitions (left, right).
    Inter8x16,
    /// Code 3 — two 16×8 partitions (top, bottom).
    Inter16x8,
    /// Code 4 — four 8×8 partitions.
    Inter8x8,
    /// Code 5 — eight 4×8 partitions.
    Inter4x8,
    /// Code 6 — eight 8×4 partitions.
    Inter8x4,
    /// Code 7 — sixteen 4×4 partitions.
    Inter4x4,
}

impl PFrameInterMode {
    /// Number of motion-vector-difference pairs the type carries.
    #[must_use]
    pub const fn num_motion_vectors(self) -> u32 {
        match self {
            Self::Skip => 0,
            Self::Inter16x16 => 1,
            Self::Inter8x16 | Self::Inter16x8 => 2,
            Self::Inter8x8 => 4,
            Self::Inter4x8 | Self::Inter8x4 => 8,
            Self::Inter4x4 => 16,
        }
    }

    /// Partition size `(width, height)` in luma samples.
    #[must_use]
    pub const fn partition_size(self) -> (u32, u32) {
        match self {
            Self::Skip | Self::Inter16x16 => (16, 16),
            Self::Inter8x16 => (8, 16),
            Self::Inter16x8 => (16, 8),
            Self::Inter8x8 => (8, 8),
            Self::Inter4x8 => (4, 8),
            Self::Inter8x4 => (8, 4),
            Self::Inter4x4 => (4, 4),
        }
    }

    /// The partitions' `(x, y)` offsets within the macroblock in coding
    /// (raster) order — spec/08 §2.
    #[must_use]
    pub fn partition_offsets(self) -> Vec<(u32, u32)> {
        let (w, h) = self.partition_size();
        let mut out = Vec::with_capacity(self.num_motion_vectors() as usize);
        if self.num_motion_vectors() == 0 {
            return out;
        }
        let mut y = 0;
        while y < 16 {
            let mut x = 0;
            while x < 16 {
                out.push((x, y));
                x += w;
            }
            y += h;
        }
        out
    }
}

/// B-slice inter macroblock dispatch (spec/07 §5; bodies not
/// specified).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BFrameInterMode {
    /// Code 0 — reads no motion data.
    Direct,
    /// Code 1 — one motion-vector pair.
    Forward,
    /// Code 2 — one motion-vector pair.
    Backward,
    /// Code 3 — two motion-vector pairs.
    Bidirectional,
}

impl BFrameInterMode {
    /// Number of explicitly coded motion-vector pairs.
    #[must_use]
    pub const fn num_motion_vectors(self) -> u32 {
        match self {
            Self::Direct => 0,
            Self::Forward | Self::Backward => 1,
            Self::Bidirectional => 2,
        }
    }
}

/// A classified macroblock type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq3MbType {
    /// An intra macroblock of any slice type.
    Intra(IntraMbKind),
    /// A P-slice inter macroblock (codes 0…7).
    PInter(PFrameInterMode),
    /// A B-slice inter macroblock (codes 0…3).
    BInter(BFrameInterMode),
}

impl Svq3MbType {
    /// `true` for the intra kinds.
    #[must_use]
    pub const fn is_intra(self) -> bool {
        matches!(self, Self::Intra(_))
    }

    /// `true` for the inter kinds (skip included).
    #[must_use]
    pub const fn is_inter(self) -> bool {
        matches!(self, Self::PInter(_) | Self::BInter(_))
    }

    /// `true` for the P-slice skip type.
    #[must_use]
    pub const fn is_skip(self) -> bool {
        matches!(self, Self::PInter(PFrameInterMode::Skip))
    }

    /// Number of motion-vector-difference pairs on the wire.
    #[must_use]
    pub const fn num_motion_vectors(self) -> u32 {
        match self {
            Self::Intra(_) => 0,
            Self::PInter(m) => m.num_motion_vectors(),
            Self::BInter(m) => m.num_motion_vectors(),
        }
    }

    /// The intra kind, if intra.
    #[must_use]
    pub const fn intra(self) -> Option<IntraMbKind> {
        match self {
            Self::Intra(k) => Some(k),
            _ => None,
        }
    }
}

fn intra16x16_from_code(code: u32, base: u32) -> Result<IntraMbKind> {
    match Intra16x16Params::from_record(code - base) {
        Some(p) => Ok(IntraMbKind::Intra16x16(p)),
        None => Err(Error::InvalidFrameCode(code)),
    }
}

/// Map a `mb_type` code number to its kind for the given slice type
/// (spec/07 §5). Out-of-range code numbers are
/// [`Error::InvalidFrameCode`].
pub fn classify_mb_type(frame_type: Svq3FrameType, code: u32) -> Result<Svq3MbType> {
    match frame_type {
        Svq3FrameType::Intra => match code {
            0 => Ok(Svq3MbType::Intra(IntraMbKind::Intra4x4)),
            1..=24 => Ok(Svq3MbType::Intra(intra16x16_from_code(code, 1)?)),
            other => Err(Error::InvalidFrameCode(other)),
        },
        Svq3FrameType::Predicted => match code {
            0 => Ok(Svq3MbType::PInter(PFrameInterMode::Skip)),
            1 => Ok(Svq3MbType::PInter(PFrameInterMode::Inter16x16)),
            2 => Ok(Svq3MbType::PInter(PFrameInterMode::Inter8x16)),
            3 => Ok(Svq3MbType::PInter(PFrameInterMode::Inter16x8)),
            4 => Ok(Svq3MbType::PInter(PFrameInterMode::Inter8x8)),
            5 => Ok(Svq3MbType::PInter(PFrameInterMode::Inter4x8)),
            6 => Ok(Svq3MbType::PInter(PFrameInterMode::Inter8x4)),
            7 => Ok(Svq3MbType::PInter(PFrameInterMode::Inter4x4)),
            P_SLICE_INTRA4X4_CODE => Ok(Svq3MbType::Intra(IntraMbKind::Intra4x4)),
            9..=32 => Ok(Svq3MbType::Intra(intra16x16_from_code(code, 9)?)),
            P_SLICE_FLAT128_CODE => Ok(Svq3MbType::Intra(IntraMbKind::Flat128)),
            other => Err(Error::InvalidFrameCode(other)),
        },
        Svq3FrameType::Bidirectional => match code {
            0 => Ok(Svq3MbType::BInter(BFrameInterMode::Direct)),
            1 => Ok(Svq3MbType::BInter(BFrameInterMode::Forward)),
            2 => Ok(Svq3MbType::BInter(BFrameInterMode::Backward)),
            3 => Ok(Svq3MbType::BInter(BFrameInterMode::Bidirectional)),
            B_SLICE_INTRA4X4_CODE => Ok(Svq3MbType::Intra(IntraMbKind::Intra4x4)),
            5..=28 => Ok(Svq3MbType::Intra(intra16x16_from_code(code, 5)?)),
            other => Err(Error::InvalidFrameCode(other)),
        },
    }
}

/// Read `uvlc mb_type` and classify it.
pub fn read_mb_type(br: &mut BitReader<'_>, frame_type: Svq3FrameType) -> Result<Svq3MbType> {
    let code = read_universal_code(br)?;
    classify_mb_type(frame_type, code)
}

/// Block index (quadrant-major, spec/07 §6 item 1) → raster cell index
/// (`row · 4 + col`) of the sixteen 4×4 luma blocks:
///
/// ```text
///   0  1 |  4  5
///   2  3 |  6  7
///   -----+------
///   8  9 | 12 13
///  10 11 | 14 15
/// ```
pub const INTRA_4X4_BLOCK_RASTER: [u8; 16] = [0, 1, 4, 5, 2, 3, 6, 7, 8, 9, 12, 13, 10, 11, 14, 15];

/// The context value of a neighbour block for `tables/08`
/// (spec/07 §10.2): 0 when the block is unavailable (outside the
/// picture, or not yet decoded in the current slice), `mode + 1` when
/// it was decoded as an intra 4×4 block, and 1 for every other decoded
/// block.
#[must_use]
pub const fn intra_4x4_context_value(neighbour: Option<u8>) -> u8 {
    match neighbour {
        None => 0,
        Some(mode) => mode + 1,
    }
}

/// The context value of a decoded block that is not intra 4×4
/// (intra 16×16, flat-128, inter, skip): `1`, i.e. DC.
pub const INTRA_4X4_CONTEXT_OTHER: u8 = 1;

/// Resolve a coded rank (0…4) into a prediction mode through the
/// `(top_context, left_context)` row of `tables/08`.
///
/// Returns [`Error::InvalidIntraPrediction`] when the table holds the
/// illegal marker for that combination (a bitstream error), and
/// [`Error::BadBitWidth`] for a context value above 5 or a rank above
/// 4 (caller bugs, not wire data).
pub fn resolve_intra_4x4_predictor(top_ctx: u8, left_ctx: u8, rank: u8) -> Result<u8> {
    if top_ctx > 5 {
        return Err(Error::BadBitWidth(top_ctx as u32));
    }
    if left_ctx > 5 {
        return Err(Error::BadBitWidth(left_ctx as u32));
    }
    if rank > 4 {
        return Err(Error::BadBitWidth(rank as u32));
    }
    let mode = SVQ3_INTRA4X4_PRED_MODE_CONTEXT[top_ctx as usize][left_ctx as usize][rank as usize];
    if mode == SVQ3_INTRA4X4_PRED_MODE_ILLEGAL {
        return Err(Error::InvalidIntraPrediction(top_ctx, left_ctx, rank));
    }
    Ok(mode)
}

/// Read one pair code (0…24) and resolve it through `tables/07` into
/// `(rank_first, rank_second)`. A code number of 25 or more is
/// [`Error::InvalidFrameCode`].
pub fn read_intra_4x4_pred_pair(br: &mut BitReader<'_>) -> Result<(u8, u8)> {
    let code = read_universal_code(br)?;
    match SVQ3_INTRA4X4_PRED_MODE_PAIRS.get(code as usize) {
        Some(&pair) => Ok(pair),
        None => Err(Error::InvalidFrameCode(code)),
    }
}

/// The sixteen resolved prediction modes of an intra 4×4 macroblock,
/// indexed by **raster cell** (`row · 4 + col`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intra4x4ModeGrid {
    modes: [u8; 16],
}

impl Intra4x4ModeGrid {
    /// The mode of the block at raster cell `index`.
    #[must_use]
    pub fn mode(&self, index: usize) -> Option<u8> {
        self.modes.get(index).copied()
    }

    /// All sixteen modes, raster order.
    #[must_use]
    pub fn modes(&self) -> &[u8; 16] {
        &self.modes
    }

    /// The context values the row of blocks below this macroblock
    /// sees as its top neighbours (`mode + 1` of cells 12…15).
    #[must_use]
    pub fn bottom_row_contexts(&self) -> [u8; 4] {
        [
            self.modes[12] + 1,
            self.modes[13] + 1,
            self.modes[14] + 1,
            self.modes[15] + 1,
        ]
    }

    /// The context values the column of blocks to the right sees as
    /// its left neighbours (`mode + 1` of cells 3, 7, 11, 15).
    #[must_use]
    pub fn right_column_contexts(&self) -> [u8; 4] {
        [
            self.modes[3] + 1,
            self.modes[7] + 1,
            self.modes[11] + 1,
            self.modes[15] + 1,
        ]
    }
}

/// Read the eight pair codes of an intra 4×4 macroblock and resolve
/// every block's rank into its mode against the neighbour contexts
/// (spec/07 §6 item 1 + §10.2).
///
/// `top_ctx[c]` is the context value of the block above the
/// macroblock's top-row block in column `c`; `left_ctx[r]` that of the
/// block to the left of the left-column block in row `r`
/// ([`intra_4x4_context_value`] / [`INTRA_4X4_CONTEXT_OTHER`]). Blocks
/// are resolved in block order, so every in-macroblock neighbour has
/// already been resolved when it is consulted.
pub fn decode_intra_4x4_modes_with_context(
    br: &mut BitReader<'_>,
    top_ctx: [u8; 4],
    left_ctx: [u8; 4],
) -> Result<Intra4x4ModeGrid> {
    let mut ranks = [0u8; 16];
    for i in 0..8 {
        let (a, b) = read_intra_4x4_pred_pair(br)?;
        ranks[2 * i] = a;
        ranks[2 * i + 1] = b;
    }
    resolve_intra_4x4_modes(&ranks, top_ctx, left_ctx)
}

/// Resolve already-read ranks (block order) into modes — the
/// resolution half of [`decode_intra_4x4_modes_with_context`].
pub fn resolve_intra_4x4_modes(
    ranks: &[u8; 16],
    top_ctx: [u8; 4],
    left_ctx: [u8; 4],
) -> Result<Intra4x4ModeGrid> {
    let mut modes = [u8::MAX; 16];
    for (block, &rank) in ranks.iter().enumerate() {
        let cell = INTRA_4X4_BLOCK_RASTER[block] as usize;
        let row = cell / 4;
        let col = cell % 4;
        let top = if row == 0 {
            top_ctx[col]
        } else {
            modes[cell - 4] + 1
        };
        let left = if col == 0 {
            left_ctx[row]
        } else {
            modes[cell - 1] + 1
        };
        modes[cell] = resolve_intra_4x4_predictor(top, left, rank)?;
    }
    Ok(Intra4x4ModeGrid { modes })
}

/// [`decode_intra_4x4_modes_with_context`] with the coarse contexts a
/// macroblock sees when its neighbours are either absent (context 0)
/// or decoded non-intra-4×4 blocks (context 1).
pub fn decode_intra_4x4_modes(
    br: &mut BitReader<'_>,
    top_avail: bool,
    left_avail: bool,
) -> Result<Intra4x4ModeGrid> {
    let ctx = |avail: bool| {
        if avail {
            [INTRA_4X4_CONTEXT_OTHER; 4]
        } else {
            [0u8; 4]
        }
    };
    decode_intra_4x4_modes_with_context(br, ctx(top_avail), ctx(left_avail))
}

/// A P-slice inter macroblock's motion-vector precision (spec/05 §3,
/// spec/08 §3; internal numbering 0 = full, 1 = half, 2 = third).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq3MvPrecision {
    /// Whole-sample vectors (stored ×6).
    Fullpel,
    /// Half-sample vectors (stored ×3).
    Halfpel,
    /// Third-sample vectors (stored ×2).
    Thirdpel,
}

/// Read the precision selector of a P-slice inter macroblock with
/// motion data (spec/08 §3): with `{full, half, third}` enabled `0` =
/// third-pel, `10` = half-pel, `11` = full-pel; with one sub-pel
/// precision enabled a single bit indexes `[full, sub-pel]`; with
/// only full-pel enabled no bits are read.
pub fn read_inter_mv_precision_p_frame(
    br: &mut BitReader<'_>,
    has_thirdpel: bool,
    has_halfpel: bool,
) -> Result<Svq3MvPrecision> {
    match (has_halfpel, has_thirdpel) {
        (true, true) => {
            if br.read_bit()? == 0 {
                Ok(Svq3MvPrecision::Thirdpel)
            } else if br.read_bit()? == 0 {
                Ok(Svq3MvPrecision::Halfpel)
            } else {
                Ok(Svq3MvPrecision::Fullpel)
            }
        }
        (true, false) => Ok(if br.read_bit()? == 1 {
            Svq3MvPrecision::Halfpel
        } else {
            Svq3MvPrecision::Fullpel
        }),
        (false, true) => Ok(if br.read_bit()? == 1 {
            Svq3MvPrecision::Thirdpel
        } else {
            Svq3MvPrecision::Fullpel
        }),
        (false, false) => Ok(Svq3MvPrecision::Fullpel),
    }
}

/// Precision selector by slice type: P slices read it
/// ([`read_inter_mv_precision_p_frame`]); B-slice bodies are not
/// specified and are given the half-pel value the wiki snapshot
/// attributes to them without consuming bits; I slices have no inter
/// macroblocks.
pub fn read_inter_mv_precision(
    br: &mut BitReader<'_>,
    frame_type: Svq3FrameType,
    has_thirdpel: bool,
    has_halfpel: bool,
) -> Result<Svq3MvPrecision> {
    match frame_type {
        Svq3FrameType::Predicted => read_inter_mv_precision_p_frame(br, has_thirdpel, has_halfpel),
        Svq3FrameType::Bidirectional => Ok(Svq3MvPrecision::Halfpel),
        Svq3FrameType::Intra => Ok(Svq3MvPrecision::Fullpel),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svq3_testutil::{pack, uvlc as ue, Packer};

    #[test]
    fn i_slice_type_numbering() {
        assert_eq!(
            classify_mb_type(Svq3FrameType::Intra, 0).unwrap(),
            Svq3MbType::Intra(IntraMbKind::Intra4x4)
        );
        // Code 1 → record 0 = (DC, chroma 0, no AC); code 24 → record 23.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Intra, 1).unwrap(),
            Svq3MbType::Intra(IntraMbKind::Intra16x16(Intra16x16Params {
                pred_mode: 0,
                cbp_chroma: 0,
                luma_ac: false
            }))
        );
        assert_eq!(
            classify_mb_type(Svq3FrameType::Intra, 24).unwrap(),
            Svq3MbType::Intra(IntraMbKind::Intra16x16(Intra16x16Params {
                pred_mode: 3,
                cbp_chroma: 2,
                luma_ac: true
            }))
        );
        assert_eq!(
            classify_mb_type(Svq3FrameType::Intra, 25).unwrap_err(),
            Error::InvalidFrameCode(25)
        );
    }

    #[test]
    fn p_slice_type_numbering() {
        assert!(classify_mb_type(Svq3FrameType::Predicted, 0)
            .unwrap()
            .is_skip());
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 7).unwrap(),
            Svq3MbType::PInter(PFrameInterMode::Inter4x4)
        );
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 8).unwrap(),
            Svq3MbType::Intra(IntraMbKind::Intra4x4)
        );
        // tables/03: 9 → (0,0,0), 12 → (3,0,0), 13 → (0,1,0), 21 → (0,0,1), 32 → (3,2,1).
        for (code, want) in [
            (9, (0, 0, false)),
            (12, (3, 0, false)),
            (13, (0, 1, false)),
            (21, (0, 0, true)),
            (32, (3, 2, true)),
        ] {
            let t = classify_mb_type(Svq3FrameType::Predicted, code).unwrap();
            let Svq3MbType::Intra(IntraMbKind::Intra16x16(p)) = t else {
                panic!("code {code} is intra 16×16");
            };
            assert_eq!((p.pred_mode, p.cbp_chroma, p.luma_ac), want, "code {code}");
            assert_eq!(p.record() + 9, code);
        }
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 33).unwrap(),
            Svq3MbType::Intra(IntraMbKind::Flat128)
        );
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 34).unwrap_err(),
            Error::InvalidFrameCode(34)
        );
    }

    #[test]
    fn b_slice_type_numbering() {
        assert_eq!(
            classify_mb_type(Svq3FrameType::Bidirectional, 3).unwrap(),
            Svq3MbType::BInter(BFrameInterMode::Bidirectional)
        );
        assert_eq!(
            classify_mb_type(Svq3FrameType::Bidirectional, 4).unwrap(),
            Svq3MbType::Intra(IntraMbKind::Intra4x4)
        );
        let t = classify_mb_type(Svq3FrameType::Bidirectional, 5).unwrap();
        assert!(matches!(
            t,
            Svq3MbType::Intra(IntraMbKind::Intra16x16(p)) if p.record() == 0
        ));
        let t = classify_mb_type(Svq3FrameType::Bidirectional, 28).unwrap();
        assert!(matches!(
            t,
            Svq3MbType::Intra(IntraMbKind::Intra16x16(p)) if p.record() == 23
        ));
        assert!(classify_mb_type(Svq3FrameType::Bidirectional, 29).is_err());
    }

    #[test]
    fn record_round_trips() {
        for r in 0..24 {
            let p = Intra16x16Params::from_record(r).unwrap();
            assert_eq!(p.record(), r);
        }
        assert!(Intra16x16Params::from_record(24).is_none());
    }

    #[test]
    fn motion_vector_counts_and_partition_layouts() {
        let cases = [
            (PFrameInterMode::Skip, 0, (16, 16)),
            (PFrameInterMode::Inter16x16, 1, (16, 16)),
            (PFrameInterMode::Inter8x16, 2, (8, 16)),
            (PFrameInterMode::Inter16x8, 2, (16, 8)),
            (PFrameInterMode::Inter8x8, 4, (8, 8)),
            (PFrameInterMode::Inter4x8, 8, (4, 8)),
            (PFrameInterMode::Inter8x4, 8, (8, 4)),
            (PFrameInterMode::Inter4x4, 16, (4, 4)),
        ];
        for (mode, n, size) in cases {
            assert_eq!(mode.num_motion_vectors(), n);
            assert_eq!(mode.partition_size(), size);
            assert_eq!(mode.partition_offsets().len(), n as usize);
        }
        // spec/08 §2 orders.
        assert_eq!(
            PFrameInterMode::Inter8x16.partition_offsets(),
            vec![(0, 0), (8, 0)]
        );
        assert_eq!(
            PFrameInterMode::Inter16x8.partition_offsets(),
            vec![(0, 0), (0, 8)]
        );
        assert_eq!(
            PFrameInterMode::Inter4x8.partition_offsets(),
            vec![
                (0, 0),
                (4, 0),
                (8, 0),
                (12, 0),
                (0, 8),
                (4, 8),
                (8, 8),
                (12, 8)
            ]
        );
        assert_eq!(
            PFrameInterMode::Inter8x4.partition_offsets(),
            vec![
                (0, 0),
                (8, 0),
                (0, 4),
                (8, 4),
                (0, 8),
                (8, 8),
                (0, 12),
                (8, 12)
            ]
        );
        assert_eq!(PFrameInterMode::Inter4x4.partition_offsets()[15], (12, 12));
        assert_eq!(BFrameInterMode::Bidirectional.num_motion_vectors(), 2);
        assert_eq!(
            Svq3MbType::PInter(PFrameInterMode::Inter8x8).num_motion_vectors(),
            4
        );
        assert_eq!(
            Svq3MbType::Intra(IntraMbKind::Intra4x4).num_motion_vectors(),
            0
        );
    }

    #[test]
    fn read_mb_type_reads_the_universal_code() {
        let bytes = pack(&[ue(0)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(
            read_mb_type(&mut br, Svq3FrameType::Intra).unwrap(),
            Svq3MbType::Intra(IntraMbKind::Intra4x4)
        );
        let bytes = pack(&[ue(33)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(
            read_mb_type(&mut br, Svq3FrameType::Predicted).unwrap(),
            Svq3MbType::Intra(IntraMbKind::Flat128)
        );
        let mut br = BitReader::new(&[]);
        assert_eq!(
            read_mb_type(&mut br, Svq3FrameType::Intra).unwrap_err(),
            Error::Truncated
        );
    }

    #[test]
    fn block_raster_is_quadrant_major() {
        let mut seen = [false; 16];
        for &cell in INTRA_4X4_BLOCK_RASTER.iter() {
            assert!(!seen[cell as usize]);
            seen[cell as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
        // Blocks 4..8 are the top-right quadrant.
        assert_eq!(&INTRA_4X4_BLOCK_RASTER[4..8], &[2, 3, 6, 7]);
    }

    #[test]
    fn context_values() {
        assert_eq!(intra_4x4_context_value(None), 0);
        assert_eq!(intra_4x4_context_value(Some(0)), 1);
        assert_eq!(intra_4x4_context_value(Some(4)), 5);
        assert_eq!(INTRA_4X4_CONTEXT_OTHER, 1);
    }

    #[test]
    fn resolve_rank_through_tables08() {
        // No neighbours: only rank 0 is legal and it is DC.
        assert_eq!(resolve_intra_4x4_predictor(0, 0, 0).unwrap(), 0);
        assert_eq!(
            resolve_intra_4x4_predictor(0, 0, 1).unwrap_err(),
            Error::InvalidIntraPrediction(0, 0, 1)
        );
        // Both neighbours DC (context 1): ranks 0..4 → 0, 2, 1, 3, 4.
        for (rank, want) in [(0, 0), (1, 2), (2, 1), (3, 3), (4, 4)] {
            assert_eq!(resolve_intra_4x4_predictor(1, 1, rank).unwrap(), want);
        }
        // Top vertical (2), left horizontal (3): 1, 2, 0, 3, 4.
        for (rank, want) in [(0, 1), (1, 2), (2, 0), (3, 3), (4, 4)] {
            assert_eq!(resolve_intra_4x4_predictor(2, 3, rank).unwrap(), want);
        }
        assert_eq!(
            resolve_intra_4x4_predictor(6, 0, 0).unwrap_err(),
            Error::BadBitWidth(6)
        );
        assert_eq!(
            resolve_intra_4x4_predictor(0, 0, 5).unwrap_err(),
            Error::BadBitWidth(5)
        );
    }

    #[test]
    fn pair_code_alphabet() {
        let bytes = pack(&[ue(24)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(read_intra_4x4_pred_pair(&mut br).unwrap(), (4, 4));
        let bytes = pack(&[ue(25)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(
            read_intra_4x4_pred_pair(&mut br).unwrap_err(),
            Error::InvalidFrameCode(25)
        );
    }

    #[test]
    fn all_rank_zero_resolves_to_dc_everywhere() {
        // spec/07 §11: eight pair codes `1` → rank 0 for all sixteen
        // blocks → DC everywhere, whatever the neighbour contexts.
        for (t, l) in [(0, 0), (1, 1), (0, 1), (1, 0), (1, 2)] {
            let mut p = Packer::new();
            for _ in 0..8 {
                p.ue(0);
            }
            let bytes = p.into_bytes();
            let mut br = BitReader::new(&bytes);
            let grid = decode_intra_4x4_modes_with_context(&mut br, [t; 4], [l; 4]).unwrap();
            assert_eq!(*grid.modes(), [0u8; 16]);
            assert_eq!(grid.bottom_row_contexts(), [1; 4]);
            assert_eq!(grid.right_column_contexts(), [1; 4]);
        }
    }

    #[test]
    fn in_macroblock_contexts_follow_block_order() {
        // Pair code 4 → ranks (1, 1). Block 0 (cell 0) with contexts
        // (1, 1) → rank 1 → mode 2 (horizontal). Block 1 (cell 1):
        // top ctx 1, left = block 0's mode + 1 = 3 → row (1, 3) rank 1
        // → mode 0. Block 2 (cell 4): top = block 0 → ctx 3, left ctx 1
        // → row (3, 1) rank 1 → 2. Block 3 (cell 5): top = cell 1 (0)
        // → ctx 1, left = cell 4 (2) → ctx 3 → row (1, 3) rank 1 → 0.
        let mut p = Packer::new();
        p.ue(4);
        p.ue(4);
        for _ in 0..6 {
            p.ue(0);
        }
        let bytes = p.into_bytes();
        let mut br = BitReader::new(&bytes);
        let grid = decode_intra_4x4_modes_with_context(&mut br, [1; 4], [1; 4]).unwrap();
        assert_eq!(grid.mode(0), Some(2));
        assert_eq!(grid.mode(1), Some(0));
        assert_eq!(grid.mode(4), Some(2));
        assert_eq!(grid.mode(5), Some(0));
        assert_eq!(grid.right_column_contexts()[0], 1);
        assert_eq!(grid.bottom_row_contexts()[0], 1);
    }

    #[test]
    fn unavailable_neighbours_reject_directional_ranks() {
        // Top-left macroblock of a picture: rank 1 for block 0 is
        // illegal (row (0, 0) admits only rank 0).
        let mut p = Packer::new();
        p.ue(4); // ranks (1, 1)
        for _ in 0..7 {
            p.ue(0);
        }
        let bytes = p.into_bytes();
        let mut br = BitReader::new(&bytes);
        assert_eq!(
            decode_intra_4x4_modes(&mut br, false, false).unwrap_err(),
            Error::InvalidIntraPrediction(0, 0, 1)
        );
        // With only the top available, block 0's row (1, 0) allows
        // ranks 0 and 1 (DC, vertical).
        let mut p = Packer::new();
        p.ue(4);
        for _ in 0..7 {
            p.ue(0);
        }
        let bytes = p.into_bytes();
        let mut br = BitReader::new(&bytes);
        let grid = decode_intra_4x4_modes(&mut br, true, false).unwrap();
        assert_eq!(grid.mode(0), Some(1));
    }

    #[test]
    fn precision_selector_forms() {
        // {full, half, third}: 0 → third, 10 → half, 11 → full.
        let bytes = pack(&[(1, 0)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(
            read_inter_mv_precision_p_frame(&mut br, true, true).unwrap(),
            Svq3MvPrecision::Thirdpel
        );
        let bytes = pack(&[(2, 0b10)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(
            read_inter_mv_precision_p_frame(&mut br, true, true).unwrap(),
            Svq3MvPrecision::Halfpel
        );
        let bytes = pack(&[(2, 0b11)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(
            read_inter_mv_precision_p_frame(&mut br, true, true).unwrap(),
            Svq3MvPrecision::Fullpel
        );
        assert_eq!(br.bits_consumed(), 2);
        // {full, half}: one bit, 1 = half.
        let bytes = pack(&[(1, 1)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(
            read_inter_mv_precision_p_frame(&mut br, false, true).unwrap(),
            Svq3MvPrecision::Halfpel
        );
        let bytes = pack(&[(1, 0)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(
            read_inter_mv_precision_p_frame(&mut br, false, true).unwrap(),
            Svq3MvPrecision::Fullpel
        );
        // {full, third}: one bit, 1 = third.
        let bytes = pack(&[(1, 1)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(
            read_inter_mv_precision_p_frame(&mut br, true, false).unwrap(),
            Svq3MvPrecision::Thirdpel
        );
        // {full}: no bits.
        let mut br = BitReader::new(&[]);
        assert_eq!(
            read_inter_mv_precision_p_frame(&mut br, false, false).unwrap(),
            Svq3MvPrecision::Fullpel
        );
        // B slices consume nothing; I slices consume nothing.
        let mut br = BitReader::new(&[]);
        assert_eq!(
            read_inter_mv_precision(&mut br, Svq3FrameType::Bidirectional, true, true).unwrap(),
            Svq3MvPrecision::Halfpel
        );
        assert_eq!(
            read_inter_mv_precision(&mut br, Svq3FrameType::Intra, true, true).unwrap(),
            Svq3MvPrecision::Fullpel
        );
    }
}
