//! SVQ3 macroblock-type tree walk (structural).
//!
//! Implements the per-slice macroblock-type classification described
//! in `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock layer"
//! and the intra-prediction pair / context-lookup tables documented
//! in §"Intra macroblock information decoding". The wiki page is a
//! verbatim local mirror of the multimedia.cx Sorenson_Video_3 wiki
//! page (fetched 2026-05-06, CC-BY-SA per multimedia.cx terms) and is
//! the only authoritative source consulted during round 4.
//!
//! ## Scope
//!
//! Round 4 is **structural**: this module walks the unsigned
//! exp-Golomb code at the start of each macroblock body, classifies it
//! by the per-slice MB-type table the wiki spec enumerates, and
//! exposes the typed [`Svq3MbType`] result. It also lands the two
//! fixed tables documented in §"Intra macroblock information
//! decoding" — the 25-entry intra-mode pair table and the
//! `6 × 6 × 5` intra-mode context-lookup table — as `pub const`
//! arrays.
//!
//! The actual macroblock payload past the MB-type code (CBP, intra
//! mode pair Golomb code, per-partition motion vectors, residual
//! coefficient stream) remains **out of round-4 scope** — those
//! sub-streams require either the H.264 CBP-coding correspondence or
//! the SVQ3-specific MV component VLC, neither of which is exercised
//! here. The structural walk is intentionally cheap and feeds the
//! decoder-handle scaffolding so a future round can attach the
//! residual / MV / intra-prediction stages.
//!
//! ## What the parser does **not** do
//!
//! * It does not decode CBP / luma intra mode / chroma DC. The wiki
//!   spec for those is either a back-reference to H.264 (for CBP /
//!   chroma DC) or unspecified beyond the structural macroblock-type
//!   identifier (intra-mode-pair Golomb walk is documented but the
//!   pair-to-mode mapping is captured here as the
//!   [`INTRA_PRED_PAIRS`] table for a later round to consume).
//! * It does not decode the per-partition motion vectors. The wiki
//!   spec §"Inter macroblock information decoding" describes the
//!   precision-selector + signed-VLC layout but the underlying VLC
//!   table is not enumerated bit-for-bit.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::svq3::{read_ue_golomb, Svq3FrameType};

/// Macroblock-type code-value range for an I-frame slice — the wiki
/// spec's §"Macroblock layer" enumerates codes `0`, `1..24`, `25`
/// (26 valid values, range `0..=25`).
pub const I_FRAME_MB_TYPE_MAX: u32 = 25;

/// Macroblock-type code-value range for a P-frame slice — `0..=33`.
/// Codes `0..=7` are inter modes; codes `8..=33` are intra modes
/// reusing the I-frame code space at an offset of `8` (so P-frame
/// type `8` corresponds to I-frame type `0` and so on).
pub const P_FRAME_MB_TYPE_MAX: u32 = 33;

/// Offset of the intra-mode code block inside the P-frame
/// macroblock-type space. P-frame intra MBs are `8..=33`; subtracting
/// [`P_FRAME_INTRA_OFFSET`] yields the equivalent I-frame intra MB
/// code value.
pub const P_FRAME_INTRA_OFFSET: u32 = 8;

/// Macroblock-type code-value range for a B-frame slice — `0..=29`.
/// Codes `0..=3` are direct / forward / backward / bidirectional
/// inter modes; codes `4..=29` are intra modes at an offset of `4`.
pub const B_FRAME_MB_TYPE_MAX: u32 = 29;

/// Offset of the intra-mode code block inside the B-frame
/// macroblock-type space.
pub const B_FRAME_INTRA_OFFSET: u32 = 4;

/// I-frame macroblock-type classification (codes `0..=25`).
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
/// layer":
///
/// * `0` — macroblock with luma DCs coded in a separate 4×4 block.
/// * `1..=24` — macroblock with a predefined coded-block-pattern and
///   intra-prediction mode (the 24 enumerated `(CBP, mode)`
///   combinations the wiki spec attaches to the H.264-style intra
///   16×16 modes).
/// * `25` — macroblock with luma DCs coded in a separate 4×4 block
///   and no other blocks coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IFrameMbType {
    /// Code `0` — luma DCs in a separate 4×4 block.
    LumaDcSeparate,
    /// Codes `1..=24` — predefined CBP + intra-prediction mode
    /// combination. The raw code value is preserved for the residual
    /// decoder to look up the matching CBP / mode pair.
    PredefinedCbpMode(u32),
    /// Code `25` — luma DCs separate, no other blocks coded.
    LumaDcSeparateNoOthers,
}

/// P-frame inter macroblock-type classification (codes `0..=7`).
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
/// layer":
///
/// | Code | Meaning |
/// | ---- | ---------------------------------------------------- |
/// | `0` | skip block |
/// | `1` | 16×16 inter block |
/// | `2` | inter block with MVs for each 8×16 part (2 MVs) |
/// | `3` | inter block with MVs for each 16×8 part (2 MVs) |
/// | `4` | inter block with MVs for each 8×8 part (4 MVs) |
/// | `5` | inter block with MVs for each 4×8 part (8 MVs) |
/// | `6` | inter block with MVs for each 8×4 part (8 MVs) |
/// | `7` | inter block with MVs for each 4×4 part (16 MVs) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PFrameInterMode {
    /// Code `0` — skip.
    Skip,
    /// Code `1` — single MV for the whole 16×16 macroblock.
    Inter16x16,
    /// Code `2` — two MVs (one per 8×16 partition).
    Inter8x16,
    /// Code `3` — two MVs (one per 16×8 partition).
    Inter16x8,
    /// Code `4` — four MVs (one per 8×8 partition).
    Inter8x8,
    /// Code `5` — eight MVs (one per 4×8 partition).
    Inter4x8,
    /// Code `6` — eight MVs (one per 8×4 partition).
    Inter8x4,
    /// Code `7` — sixteen MVs (one per 4×4 partition).
    Inter4x4,
}

impl PFrameInterMode {
    /// Number of motion vectors this inter mode carries. Per the wiki
    /// spec table — 0 for SKIP, 1 for 16×16, 2 for 8×16 / 16×8,
    /// 4 for 8×8, 8 for 4×8 / 8×4, 16 for 4×4.
    pub fn num_motion_vectors(self) -> u32 {
        match self {
            Self::Skip => 0,
            Self::Inter16x16 => 1,
            Self::Inter8x16 | Self::Inter16x8 => 2,
            Self::Inter8x8 => 4,
            Self::Inter4x8 | Self::Inter8x4 => 8,
            Self::Inter4x4 => 16,
        }
    }
}

/// B-frame inter macroblock-type classification (codes `0..=3`).
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
/// layer":
///
/// | Code | Meaning |
/// | ---- | ---------------------------------------------------- |
/// | `0` | direct block (MV per 4×4 block from next-ref + frame distance) with coded residue |
/// | `1` | forward block |
/// | `2` | backward block |
/// | `3` | bidirectionally predicted block (codes two MVs) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BFrameInterMode {
    /// Code `0` — direct mode. MV per 4×4 block is derived from the
    /// next reference frame's MV plus frame distance.
    Direct,
    /// Code `1` — single forward MV.
    Forward,
    /// Code `2` — single backward MV.
    Backward,
    /// Code `3` — bidirectional. Two MVs (one forward, one backward).
    Bidirectional,
}

impl BFrameInterMode {
    /// Number of explicitly-coded motion vectors in the slice body.
    /// `Direct` carries no on-wire MV (it's derived); `Bidirectional`
    /// carries two; the unidirectional modes carry one.
    pub fn num_motion_vectors(self) -> u32 {
        match self {
            Self::Direct => 0,
            Self::Forward | Self::Backward => 1,
            Self::Bidirectional => 2,
        }
    }
}

/// Per-slice typed macroblock classification result.
///
/// Constructed by [`read_mb_type`]. The variant depends on the
/// enclosing slice's [`Svq3FrameType`] (the macroblock-type code
/// space is different per slice type per the wiki spec table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq3MbType {
    /// I-frame slice macroblock (codes `0..=25`).
    IIntra(IFrameMbType),
    /// P-frame slice inter macroblock (codes `0..=7`).
    PInter(PFrameInterMode),
    /// P-frame slice intra macroblock (codes `8..=33`, mapped to the
    /// equivalent I-frame intra MB type after subtracting the
    /// [`P_FRAME_INTRA_OFFSET`]).
    PIntra(IFrameMbType),
    /// B-frame slice inter macroblock (codes `0..=3`).
    BInter(BFrameInterMode),
    /// B-frame slice intra macroblock (codes `4..=29`, mapped via
    /// the [`B_FRAME_INTRA_OFFSET`]).
    BIntra(IFrameMbType),
}

impl Svq3MbType {
    /// `true` iff this macroblock is intra-coded in the underlying
    /// I-frame code space. Useful for the residual decoder which
    /// needs to know whether to perform intra-prediction or motion
    /// compensation.
    pub fn is_intra(self) -> bool {
        matches!(self, Self::IIntra(_) | Self::PIntra(_) | Self::BIntra(_))
    }

    /// `true` iff this macroblock is inter-coded (P-frame `0..=7` or
    /// B-frame `0..=3`).
    pub fn is_inter(self) -> bool {
        matches!(self, Self::PInter(_) | Self::BInter(_))
    }

    /// `true` iff this macroblock is the P-frame SKIP type
    /// (code `0` in a P-frame slice). The wiki spec describes SKIP
    /// as "the block remains unchanged" — its body is empty.
    pub fn is_skip(self) -> bool {
        matches!(self, Self::PInter(PFrameInterMode::Skip))
    }

    /// Number of motion vectors this macroblock carries on the wire.
    /// `0` for intra MBs / SKIP / B-direct, see
    /// [`PFrameInterMode::num_motion_vectors`] and
    /// [`BFrameInterMode::num_motion_vectors`] for the inter modes.
    pub fn num_motion_vectors(self) -> u32 {
        match self {
            Self::IIntra(_) | Self::PIntra(_) | Self::BIntra(_) => 0,
            Self::PInter(m) => m.num_motion_vectors(),
            Self::BInter(m) => m.num_motion_vectors(),
        }
    }

    /// Underlying I-frame intra MB type, if this is an intra MB. Lets
    /// the residual decoder reuse a single I-frame intra classifier
    /// regardless of which slice carried the macroblock.
    pub fn intra(self) -> Option<IFrameMbType> {
        match self {
            Self::IIntra(t) | Self::PIntra(t) | Self::BIntra(t) => Some(t),
            _ => None,
        }
    }
}

/// Construct an [`IFrameMbType`] from a raw I-frame code value. Used
/// internally by [`read_mb_type`] and by the P-frame / B-frame
/// classifier helpers when they peel off the intra-offset and
/// re-classify the residue against the I-frame code space.
fn classify_i_code(code: u32) -> Result<IFrameMbType> {
    match code {
        0 => Ok(IFrameMbType::LumaDcSeparate),
        1..=24 => Ok(IFrameMbType::PredefinedCbpMode(code)),
        25 => Ok(IFrameMbType::LumaDcSeparateNoOthers),
        other => Err(Error::InvalidFrameCode(other)),
    }
}

/// Classify a raw P-frame code-`0..=7` value into a
/// [`PFrameInterMode`]. Invalid values (above `7`) return
/// [`Error::InvalidFrameCode`].
fn classify_p_inter_code(code: u32) -> Result<PFrameInterMode> {
    match code {
        0 => Ok(PFrameInterMode::Skip),
        1 => Ok(PFrameInterMode::Inter16x16),
        2 => Ok(PFrameInterMode::Inter8x16),
        3 => Ok(PFrameInterMode::Inter16x8),
        4 => Ok(PFrameInterMode::Inter8x8),
        5 => Ok(PFrameInterMode::Inter4x8),
        6 => Ok(PFrameInterMode::Inter8x4),
        7 => Ok(PFrameInterMode::Inter4x4),
        other => Err(Error::InvalidFrameCode(other)),
    }
}

/// Classify a raw B-frame code-`0..=3` value into a
/// [`BFrameInterMode`]. Invalid values (above `3`) return
/// [`Error::InvalidFrameCode`].
fn classify_b_inter_code(code: u32) -> Result<BFrameInterMode> {
    match code {
        0 => Ok(BFrameInterMode::Direct),
        1 => Ok(BFrameInterMode::Forward),
        2 => Ok(BFrameInterMode::Backward),
        3 => Ok(BFrameInterMode::Bidirectional),
        other => Err(Error::InvalidFrameCode(other)),
    }
}

/// Classify a Golomb-decoded MB-type code value against the enclosing
/// slice's frame type. Equivalent to [`read_mb_type`] but takes a
/// pre-decoded code so callers that need to expose the raw value (for
/// follow-up CBP / mode-pair work) can avoid a re-read.
///
/// Returns [`Error::InvalidFrameCode`] when `code` is outside the
/// per-slice valid range.
pub fn classify_mb_type(frame_type: Svq3FrameType, code: u32) -> Result<Svq3MbType> {
    match frame_type {
        Svq3FrameType::Intra => {
            if code > I_FRAME_MB_TYPE_MAX {
                return Err(Error::InvalidFrameCode(code));
            }
            Ok(Svq3MbType::IIntra(classify_i_code(code)?))
        }
        Svq3FrameType::Predicted => {
            if code > P_FRAME_MB_TYPE_MAX {
                return Err(Error::InvalidFrameCode(code));
            }
            if code < P_FRAME_INTRA_OFFSET {
                Ok(Svq3MbType::PInter(classify_p_inter_code(code)?))
            } else {
                Ok(Svq3MbType::PIntra(classify_i_code(
                    code - P_FRAME_INTRA_OFFSET,
                )?))
            }
        }
        Svq3FrameType::Bidirectional => {
            if code > B_FRAME_MB_TYPE_MAX {
                return Err(Error::InvalidFrameCode(code));
            }
            if code < B_FRAME_INTRA_OFFSET {
                Ok(Svq3MbType::BInter(classify_b_inter_code(code)?))
            } else {
                Ok(Svq3MbType::BIntra(classify_i_code(
                    code - B_FRAME_INTRA_OFFSET,
                )?))
            }
        }
    }
}

/// Read an unsigned exp-Golomb code from the bit-reader and classify
/// it against the supplied slice frame type. Equivalent to calling
/// [`read_ue_golomb`] followed by [`classify_mb_type`].
///
/// Returns [`Error::InvalidFrameCode`] when the decoded code value
/// is outside the per-slice valid range and [`Error::Truncated`]
/// when the bit-reader runs out of bits mid-code.
pub fn read_mb_type(br: &mut BitReader<'_>, frame_type: Svq3FrameType) -> Result<Svq3MbType> {
    let code = read_ue_golomb(br)?;
    classify_mb_type(frame_type, code)
}

/// The 25-entry intra-mode pair lookup table for 4×4 intra
/// prediction.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra macroblock
/// information decoding": "Prediction is performed by reading a
/// variable-length code which corresponds to one of the following
/// pairs". The wiki spec lists the 25 pairs in a triangular layout
/// `{ 0, 0 }; { 1, 0 }, { 0, 1 }; …; { 4, 4 }`; the array here
/// preserves that listing order so the Golomb-decoded index maps
/// directly to the entry.
///
/// Each pair is `(top, left)` — the wiki spec uses this naming when
/// it describes the [`INTRA_PRED_TABLE`] lookup as
/// `pred_table[top + 1][left + 1][idx]`.
pub const INTRA_PRED_PAIRS: [(u8, u8); 25] = [
    (0, 0),
    (1, 0),
    (0, 1),
    (0, 2),
    (1, 1),
    (2, 0),
    (3, 0),
    (2, 1),
    (1, 2),
    (0, 3),
    (0, 4),
    (1, 3),
    (2, 2),
    (3, 1),
    (4, 0),
    (4, 1),
    (3, 2),
    (2, 3),
    (1, 4),
    (2, 4),
    (3, 3),
    (4, 2),
    (4, 3),
    (3, 4),
    (4, 4),
];

/// Intra-mode context-lookup table.
///
/// Per the wiki spec §"Intra macroblock information decoding": "Each
/// element of the pair is then used as an index in the prediction
/// table (the proper order is `pred_table[top + 1][left + 1][idx]`).
/// When predictors lie outside of slice, `-1` is used instead. For
/// 16×16 intra and any inter blocks value of `2` is used as the
/// predictor."
///
/// The table is shaped `[top + 1][left + 1][idx]` and stored as
/// `i8` so the `-1` sentinel (out-of-band / undefined entry per the
/// spec) is representable. The first index spans `top ∈ -1..=4`
/// (i.e. `top + 1 ∈ 0..=5`); the second spans `left ∈ -1..=4` /
/// `left + 1 ∈ 0..=5`; the inner array carries up to 5 candidate
/// predictor indices for the chosen `(top, left)` context.
///
/// Round 4 lands the table verbatim from the wiki spec for use by a
/// future intra-prediction stage — the structural-only MB-type walk
/// does not itself index this table.
pub const INTRA_PRED_TABLE: [[[i8; 5]; 6]; 6] = [
    [
        [2, -1, -1, -1, -1],
        [2, 1, -1, -1, -1],
        [1, 2, -1, -1, -1],
        [2, 1, -1, -1, -1],
        [1, 2, -1, -1, -1],
        [1, 2, -1, -1, -1],
    ],
    [
        [0, 2, -1, -1, -1],
        [0, 2, 1, 4, 3],
        [0, 1, 2, 4, 3],
        [0, 2, 1, 4, 3],
        [2, 0, 1, 3, 4],
        [0, 4, 2, 1, 3],
    ],
    [
        [2, 0, -1, -1, -1],
        [2, 1, 0, 4, 3],
        [1, 2, 4, 0, 3],
        [2, 1, 0, 4, 3],
        [2, 1, 4, 3, 0],
        [1, 2, 4, 0, 3],
    ],
    [
        [2, 0, -1, -1, -1],
        [2, 0, 1, 4, 3],
        [1, 2, 0, 4, 3],
        [2, 1, 0, 4, 3],
        [2, 1, 3, 4, 0],
        [2, 4, 1, 0, 3],
    ],
    [
        [0, 2, -1, -1, -1],
        [0, 2, 1, 3, 4],
        [1, 2, 3, 0, 4],
        [2, 0, 1, 3, 4],
        [2, 1, 3, 0, 4],
        [2, 0, 4, 3, 1],
    ],
    [
        [0, 2, -1, -1, -1],
        [0, 2, 4, 1, 3],
        [1, 4, 2, 0, 3],
        [4, 2, 0, 1, 3],
        [2, 0, 1, 4, 3],
        [4, 2, 1, 0, 3],
    ],
];

/// 16-entry scan order for the 4×4 luma intra-prediction sub-blocks.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra macroblock
/// information decoding" the scan order is:
///
/// ```text
///   ( 0,  1)  ( 4,  5)
///   ( 2,  3)  ( 6,  7)
///   ( 8,  9)  (12, 13)
///   (10, 11)  (14, 15)
/// ```
///
/// reading row-major; the array values are the 4×4-block indices the
/// decoder processes in order. Round 4 lands this for future use by
/// the intra-prediction stage. The values are exactly the raster
/// indices written out left-to-right, top-to-bottom from the wiki
/// spec block.
pub const INTRA_4X4_SCAN_ORDER: [u8; 16] = [0, 1, 4, 5, 2, 3, 6, 7, 8, 9, 12, 13, 10, 11, 14, 15];

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: pack a sequence of `(width, value)` items into a byte
    /// stream by writing them MSB-first. Mirrors the helper used by
    /// `svq3::tests` so the MB-type tests can build fixtures without
    /// re-implementing the bit-packing.
    fn pack(items: &[(u32, u32)]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut bit_cursor: usize = 0;
        for &(width, value) in items {
            assert!((1..=32).contains(&width));
            assert!(width == 32 || value < (1u32 << width));
            for i in (0..width).rev() {
                let bit = ((value >> i) & 1) as u8;
                let byte_idx = bit_cursor / 8;
                if byte_idx >= out.len() {
                    out.push(0);
                }
                let shift = 7 - (bit_cursor % 8);
                out[byte_idx] |= bit << shift;
                bit_cursor += 1;
            }
        }
        out
    }

    /// `ue(n)` exp-Golomb encoding helper — produces a `(width,
    /// value)` pack item that decodes to `n`.
    ///
    /// `ue(0) = "1"`, `ue(1) = "010"`, `ue(2) = "011"`,
    /// `ue(3) = "00100"`, …, `ue(n) = leading_zeros(p) zero bits +
    /// "1" + suffix` where `p = n + 1`.
    fn ue(n: u32) -> (u32, u32) {
        let p = n + 1;
        let leading = 31 - p.leading_zeros();
        let width = 2 * leading + 1;
        (width, p)
    }

    #[test]
    fn i_frame_code_table() {
        // Code 0 → LumaDcSeparate.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Intra, 0).unwrap(),
            Svq3MbType::IIntra(IFrameMbType::LumaDcSeparate)
        );
        // Codes 1..=24 → PredefinedCbpMode.
        for code in 1..=24u32 {
            let got = classify_mb_type(Svq3FrameType::Intra, code).unwrap();
            assert_eq!(
                got,
                Svq3MbType::IIntra(IFrameMbType::PredefinedCbpMode(code))
            );
        }
        // Code 25 → LumaDcSeparateNoOthers.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Intra, 25).unwrap(),
            Svq3MbType::IIntra(IFrameMbType::LumaDcSeparateNoOthers)
        );
    }

    #[test]
    fn i_frame_rejects_out_of_range_code() {
        let err = classify_mb_type(Svq3FrameType::Intra, 26).unwrap_err();
        assert!(matches!(err, Error::InvalidFrameCode(26)));
        let err = classify_mb_type(Svq3FrameType::Intra, 1000).unwrap_err();
        assert!(matches!(err, Error::InvalidFrameCode(1000)));
    }

    #[test]
    fn p_frame_inter_codes() {
        let expected: &[(u32, PFrameInterMode)] = &[
            (0, PFrameInterMode::Skip),
            (1, PFrameInterMode::Inter16x16),
            (2, PFrameInterMode::Inter8x16),
            (3, PFrameInterMode::Inter16x8),
            (4, PFrameInterMode::Inter8x8),
            (5, PFrameInterMode::Inter4x8),
            (6, PFrameInterMode::Inter8x4),
            (7, PFrameInterMode::Inter4x4),
        ];
        for &(code, mode) in expected {
            assert_eq!(
                classify_mb_type(Svq3FrameType::Predicted, code).unwrap(),
                Svq3MbType::PInter(mode)
            );
        }
    }

    #[test]
    fn p_frame_intra_codes_offset_correctly() {
        // P-frame code 8 ↔ I-frame code 0 (LumaDcSeparate).
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 8).unwrap(),
            Svq3MbType::PIntra(IFrameMbType::LumaDcSeparate)
        );
        // P-frame code 9 ↔ I-frame code 1.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 9).unwrap(),
            Svq3MbType::PIntra(IFrameMbType::PredefinedCbpMode(1))
        );
        // P-frame code 32 ↔ I-frame code 24.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 32).unwrap(),
            Svq3MbType::PIntra(IFrameMbType::PredefinedCbpMode(24))
        );
        // P-frame code 33 ↔ I-frame code 25 (LumaDcSeparateNoOthers).
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 33).unwrap(),
            Svq3MbType::PIntra(IFrameMbType::LumaDcSeparateNoOthers)
        );
    }

    #[test]
    fn p_frame_rejects_out_of_range_code() {
        let err = classify_mb_type(Svq3FrameType::Predicted, 34).unwrap_err();
        assert!(matches!(err, Error::InvalidFrameCode(34)));
    }

    #[test]
    fn b_frame_inter_codes() {
        let expected: &[(u32, BFrameInterMode)] = &[
            (0, BFrameInterMode::Direct),
            (1, BFrameInterMode::Forward),
            (2, BFrameInterMode::Backward),
            (3, BFrameInterMode::Bidirectional),
        ];
        for &(code, mode) in expected {
            assert_eq!(
                classify_mb_type(Svq3FrameType::Bidirectional, code).unwrap(),
                Svq3MbType::BInter(mode)
            );
        }
    }

    #[test]
    fn b_frame_intra_codes_offset_correctly() {
        // B-frame code 4 ↔ I-frame code 0.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Bidirectional, 4).unwrap(),
            Svq3MbType::BIntra(IFrameMbType::LumaDcSeparate)
        );
        // B-frame code 29 ↔ I-frame code 25.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Bidirectional, 29).unwrap(),
            Svq3MbType::BIntra(IFrameMbType::LumaDcSeparateNoOthers)
        );
    }

    #[test]
    fn b_frame_rejects_out_of_range_code() {
        let err = classify_mb_type(Svq3FrameType::Bidirectional, 30).unwrap_err();
        assert!(matches!(err, Error::InvalidFrameCode(30)));
    }

    #[test]
    fn p_inter_num_motion_vectors() {
        assert_eq!(PFrameInterMode::Skip.num_motion_vectors(), 0);
        assert_eq!(PFrameInterMode::Inter16x16.num_motion_vectors(), 1);
        assert_eq!(PFrameInterMode::Inter8x16.num_motion_vectors(), 2);
        assert_eq!(PFrameInterMode::Inter16x8.num_motion_vectors(), 2);
        assert_eq!(PFrameInterMode::Inter8x8.num_motion_vectors(), 4);
        assert_eq!(PFrameInterMode::Inter4x8.num_motion_vectors(), 8);
        assert_eq!(PFrameInterMode::Inter8x4.num_motion_vectors(), 8);
        assert_eq!(PFrameInterMode::Inter4x4.num_motion_vectors(), 16);
    }

    #[test]
    fn b_inter_num_motion_vectors() {
        assert_eq!(BFrameInterMode::Direct.num_motion_vectors(), 0);
        assert_eq!(BFrameInterMode::Forward.num_motion_vectors(), 1);
        assert_eq!(BFrameInterMode::Backward.num_motion_vectors(), 1);
        assert_eq!(BFrameInterMode::Bidirectional.num_motion_vectors(), 2);
    }

    #[test]
    fn predicates_match_intra_inter_skip() {
        let intra = Svq3MbType::IIntra(IFrameMbType::LumaDcSeparate);
        assert!(intra.is_intra());
        assert!(!intra.is_inter());
        assert!(!intra.is_skip());

        let p_intra = Svq3MbType::PIntra(IFrameMbType::LumaDcSeparate);
        assert!(p_intra.is_intra());
        assert!(!p_intra.is_inter());

        let p_skip = Svq3MbType::PInter(PFrameInterMode::Skip);
        assert!(!p_skip.is_intra());
        assert!(p_skip.is_inter());
        assert!(p_skip.is_skip());

        let p_16x16 = Svq3MbType::PInter(PFrameInterMode::Inter16x16);
        assert!(!p_16x16.is_intra());
        assert!(p_16x16.is_inter());
        assert!(!p_16x16.is_skip());

        let b_bidir = Svq3MbType::BInter(BFrameInterMode::Bidirectional);
        assert!(b_bidir.is_inter());
        assert_eq!(b_bidir.num_motion_vectors(), 2);
    }

    #[test]
    fn intra_helper_extracts_underlying_i_type() {
        let i = Svq3MbType::IIntra(IFrameMbType::PredefinedCbpMode(7));
        assert_eq!(i.intra(), Some(IFrameMbType::PredefinedCbpMode(7)));
        let p_i = Svq3MbType::PIntra(IFrameMbType::PredefinedCbpMode(7));
        assert_eq!(p_i.intra(), Some(IFrameMbType::PredefinedCbpMode(7)));
        let b_i = Svq3MbType::BIntra(IFrameMbType::PredefinedCbpMode(7));
        assert_eq!(b_i.intra(), Some(IFrameMbType::PredefinedCbpMode(7)));
        let p_inter = Svq3MbType::PInter(PFrameInterMode::Inter16x16);
        assert_eq!(p_inter.intra(), None);
        let b_inter = Svq3MbType::BInter(BFrameInterMode::Forward);
        assert_eq!(b_inter.intra(), None);
    }

    #[test]
    fn intra_mb_carries_zero_motion_vectors() {
        for frame in [
            Svq3FrameType::Intra,
            Svq3FrameType::Predicted,
            Svq3FrameType::Bidirectional,
        ] {
            // Walk every valid code, ensure intra ones carry zero MVs.
            let max = match frame {
                Svq3FrameType::Intra => I_FRAME_MB_TYPE_MAX,
                Svq3FrameType::Predicted => P_FRAME_MB_TYPE_MAX,
                Svq3FrameType::Bidirectional => B_FRAME_MB_TYPE_MAX,
            };
            for code in 0..=max {
                let mb = classify_mb_type(frame, code).unwrap();
                if mb.is_intra() {
                    assert_eq!(
                        mb.num_motion_vectors(),
                        0,
                        "intra MB code {code} in {frame:?} reports {} MVs",
                        mb.num_motion_vectors()
                    );
                }
            }
        }
    }

    #[test]
    fn read_mb_type_decodes_golomb_for_i_frame() {
        // ue(0) = "1" → code 0 → LumaDcSeparate
        let bytes = pack(&[(1, 0b1)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Intra).unwrap();
        assert_eq!(mb, Svq3MbType::IIntra(IFrameMbType::LumaDcSeparate));

        // ue(25) = a 5-bit prefix of leading zeros + 1 followed by 4
        // suffix bits. p = 26 = 0b11010, leading=4, width=9 → bits
        // "00001 1010" → 0b000011010 = 0x1A.
        let (w, v) = ue(25);
        assert_eq!(w, 9);
        assert_eq!(v, 26);
        let bytes = pack(&[(w, v)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Intra).unwrap();
        assert_eq!(mb, Svq3MbType::IIntra(IFrameMbType::LumaDcSeparateNoOthers));
    }

    #[test]
    fn read_mb_type_decodes_golomb_for_p_frame() {
        // ue(0) = "1" → P-skip
        let bytes = pack(&[ue(0)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Predicted).unwrap();
        assert_eq!(mb, Svq3MbType::PInter(PFrameInterMode::Skip));

        // ue(7) → P-Inter4x4
        let bytes = pack(&[ue(7)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Predicted).unwrap();
        assert_eq!(mb, Svq3MbType::PInter(PFrameInterMode::Inter4x4));

        // ue(8) → P-intra of code 0 (LumaDcSeparate).
        let bytes = pack(&[ue(8)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Predicted).unwrap();
        assert_eq!(mb, Svq3MbType::PIntra(IFrameMbType::LumaDcSeparate));
    }

    #[test]
    fn read_mb_type_decodes_golomb_for_b_frame() {
        // ue(0) → B-Direct.
        let bytes = pack(&[ue(0)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Bidirectional).unwrap();
        assert_eq!(mb, Svq3MbType::BInter(BFrameInterMode::Direct));

        // ue(3) → B-Bidirectional.
        let bytes = pack(&[ue(3)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Bidirectional).unwrap();
        assert_eq!(mb, Svq3MbType::BInter(BFrameInterMode::Bidirectional));

        // ue(4) → B-intra of code 0.
        let bytes = pack(&[ue(4)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Bidirectional).unwrap();
        assert_eq!(mb, Svq3MbType::BIntra(IFrameMbType::LumaDcSeparate));
    }

    #[test]
    fn read_mb_type_rejects_out_of_range_golomb_for_i_frame() {
        // ue(26) → out of I-frame range.
        let bytes = pack(&[ue(26)]);
        let mut br = BitReader::new(&bytes);
        let err = read_mb_type(&mut br, Svq3FrameType::Intra).unwrap_err();
        assert!(matches!(err, Error::InvalidFrameCode(26)));
    }

    #[test]
    fn read_mb_type_rejects_truncated_input() {
        // Empty input — Golomb decode fails with Truncated before it
        // can classify.
        let mut br = BitReader::new(&[]);
        let err = read_mb_type(&mut br, Svq3FrameType::Intra).unwrap_err();
        assert!(matches!(err, Error::Truncated));
    }

    #[test]
    fn intra_pred_pairs_table_invariants() {
        // 25 entries.
        assert_eq!(INTRA_PRED_PAIRS.len(), 25);
        // Each component is in 0..=4.
        for &(top, left) in &INTRA_PRED_PAIRS {
            assert!(top <= 4, "top {top} out of range");
            assert!(left <= 4, "left {left} out of range");
        }
        // First entry is the wiki-spec leading {0,0}.
        assert_eq!(INTRA_PRED_PAIRS[0], (0, 0));
        // Last entry is the wiki-spec trailing {4,4}.
        assert_eq!(INTRA_PRED_PAIRS[24], (4, 4));
        // The wiki spec's row 2 (after {0,0}; {1,0}, {0,1}) is
        // {0,2}, {1,1}, {2,0}.
        assert_eq!(INTRA_PRED_PAIRS[3], (0, 2));
        assert_eq!(INTRA_PRED_PAIRS[4], (1, 1));
        assert_eq!(INTRA_PRED_PAIRS[5], (2, 0));
        // Row 5 (the wiki "{0,4}, {1,3}, {2,2}, {3,1}, {4,0}").
        assert_eq!(INTRA_PRED_PAIRS[10], (0, 4));
        assert_eq!(INTRA_PRED_PAIRS[14], (4, 0));
    }

    #[test]
    fn intra_pred_pairs_table_has_no_duplicates() {
        // Every (top, left) pair in the wiki spec's triangle is
        // unique — verify the array doesn't accidentally repeat one.
        let mut seen = std::collections::HashSet::new();
        for &pair in &INTRA_PRED_PAIRS {
            assert!(seen.insert(pair), "duplicate {pair:?}");
        }
    }

    #[test]
    fn intra_pred_table_shape() {
        // 6 × 6 × 5 per the wiki spec.
        assert_eq!(INTRA_PRED_TABLE.len(), 6);
        for row in &INTRA_PRED_TABLE {
            assert_eq!(row.len(), 6);
            for inner in row {
                assert_eq!(inner.len(), 5);
            }
        }
    }

    #[test]
    fn intra_pred_table_known_sentinels() {
        // Row 0 (top = -1): the wiki spec shows two-element rows
        // padded with -1 sentinels. e.g. [0][0] = {2,-1,-1,-1,-1}.
        assert_eq!(INTRA_PRED_TABLE[0][0], [2, -1, -1, -1, -1]);
        assert_eq!(INTRA_PRED_TABLE[0][1], [2, 1, -1, -1, -1]);

        // Row 1 [1][1]: {0,2,1,4,3}
        assert_eq!(INTRA_PRED_TABLE[1][1], [0, 2, 1, 4, 3]);

        // Last row [5][5]: {4,2,1,0,3}
        assert_eq!(INTRA_PRED_TABLE[5][5], [4, 2, 1, 0, 3]);

        // Sanity: every non-sentinel entry must be in 0..=4 and -1
        // is the only allowed sentinel. Verify across the whole
        // table.
        for r in &INTRA_PRED_TABLE {
            for inner in r {
                for &v in inner {
                    assert!(
                        v == -1 || (0..=4).contains(&v),
                        "table value {v} outside {{-1, 0..=4}}"
                    );
                }
            }
        }
    }

    #[test]
    fn intra_4x4_scan_order_is_a_permutation_of_0_to_15() {
        // The scan order must be a permutation of 0..=15.
        let mut seen = [false; 16];
        for &v in &INTRA_4X4_SCAN_ORDER {
            assert!(!seen[v as usize], "duplicate {v}");
            seen[v as usize] = true;
        }
        for (i, present) in seen.iter().enumerate() {
            assert!(*present, "missing scan-order index {i}");
        }
        // Wiki spec's explicit first / last values.
        assert_eq!(INTRA_4X4_SCAN_ORDER[0], 0);
        assert_eq!(INTRA_4X4_SCAN_ORDER[15], 15);
        // Wiki spec row 1: (0, 1, 4, 5).
        assert_eq!(&INTRA_4X4_SCAN_ORDER[0..4], &[0, 1, 4, 5]);
        // Wiki spec row 4: (10, 11, 14, 15).
        assert_eq!(&INTRA_4X4_SCAN_ORDER[12..16], &[10, 11, 14, 15]);
    }

    #[test]
    fn constants_match_wiki_table() {
        assert_eq!(I_FRAME_MB_TYPE_MAX, 25);
        assert_eq!(P_FRAME_MB_TYPE_MAX, 33);
        assert_eq!(B_FRAME_MB_TYPE_MAX, 29);
        assert_eq!(P_FRAME_INTRA_OFFSET, 8);
        assert_eq!(B_FRAME_INTRA_OFFSET, 4);
    }
}
