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
//! Round 4 is **structural**: this module walks the universal-code
//! codeword at the start of each macroblock body, classifies it
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
use crate::svq3::{read_universal_code, Svq3FrameType};

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

/// The decoded parameter triple of an intra 16×16 macroblock type.
///
/// Per `docs/video/svq3/spec/04-dc-secondary-transform.md` §4.5 (and
/// `docs/video/svq3/tables/03-intra16x16-macroblock-types.csv`) the
/// twenty-four unified macroblock type code numbers 9…32 factor
/// exactly as
///
/// ```text
/// mb_type = 9 + intra16x16_pred_mode + 4 · cbp_chroma + 12 · luma_ac
/// ```
///
/// There is **no coded-block-pattern element on the wire** for these
/// types: the luma pattern is the single bit `luma_ac` (all sixteen
/// blocks or none) and the chroma pattern is the three-valued class of
/// spec/03 §1.2, both carried by the type itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intra16x16Params {
    /// The 16×16 luma prediction mode selector, `0..=3`. (The binding
    /// of the four values to the four predictors is not pinned by the
    /// staged docs — see `docs/video/svq3/provenance/05` "What was NOT
    /// established".)
    pub pred_mode: u8,
    /// The chroma class of spec/03 §1.2: 0 = no chroma residual,
    /// 1 = chroma DC only, 2 = chroma DC and AC.
    pub cbp_chroma: u8,
    /// Whether the sixteen luma 4×4 blocks carry AC coefficients
    /// (spec/04 §4.3). The separate luma DC block is present either
    /// way.
    pub luma_ac: bool,
}

impl Intra16x16Params {
    /// Factor a unified macroblock type code number `9..=32` into its
    /// parameter triple. Returns `None` outside that range.
    #[must_use]
    pub const fn from_unified_type(t: u32) -> Option<Self> {
        if t < 9 || t > 32 {
            return None;
        }
        let idx = t - 9;
        Some(Self {
            pred_mode: (idx % 4) as u8,
            cbp_chroma: ((idx / 4) % 3) as u8,
            luma_ac: idx / 12 == 1,
        })
    }

    /// The inverse of [`Self::from_unified_type`].
    #[must_use]
    pub const fn unified_type(self) -> u32 {
        9 + self.pred_mode as u32 + 4 * self.cbp_chroma as u32 + 12 * self.luma_ac as u32
    }
}

/// Intra macroblock-type classification, shared by all three frame
/// types' intra code ranges.
///
/// The 26-entry intra type space is the wiki §"Macroblock layer"
/// I-frame list (`0..=25`); P-frame intra codes are `8..=33` and
/// B-frame intra codes are `4..=29`, each "the same as above" at the
/// respective offset per the wiki.
///
/// **I-frame wire mapping (fixture-pinned, r446).** The staged
/// binary-anchored chapters give the decoder's *dispatch* numbering
/// (spec/04 §4.5: below 9 inter, 9…32 intra 16×16, 33 intra 4×4) but
/// not the per-slice-type wire-code adjustment. The all-intra sync
/// frame of `docs/video/svq3/fixtures/real-sample-320x240-short-seqh`
/// (a uniform-black 300-macroblock I-frame) pins the I-frame wire
/// mapping structurally: its macroblock layer is 299 identical
/// 14-bit units — one universal code `0`, eight 1-bit prediction-pair
/// codes, and the 5-bit code `3` (whose intra CBP mapping is pattern
/// `0`, spec/03 §4) — ending bit-exactly at the slice boundary. Only
/// wire code 0 = the **intra 4×4** grammar (pairs + explicit CBP)
/// tiles that stream, so:
///
/// * wire `0` → intra 4×4 (dispatch 33): per-4×4 modes + explicit
///   CBP, luma DCs inline;
/// * wire `1..=24` → the 24 intra 16×16 records (dispatch `wire + 8`,
///   the tables/03 factorisation): implied CBP, separate luma DC
///   block (spec/04 §4);
/// * wire `25` → by elimination the remaining dispatch value `8` —
///   the wiki list's "luma DCs coded in a separate 4×4 block and no
///   other blocks coded" type ([`Self::SeparateDcOnly`]). Its
///   reconstruction grammar beyond the DC block is not yet pinned by
///   the staged docs.
///
/// **r450 caveat.** Black-box body-shape probing of the reference
/// decoder (see the [`crate::svq3_frame`] module docs) confirms wire
/// `0`'s pairs + explicit-CBP shape but shows the `1..=24` bodies do
/// **not** fit the tables/03 factoring under any constant offset
/// (probed minimal bodies: types 4, 9, 10, 19–22 take one coefficient
/// list, 12 and 25 take three, 1–2 carry mode-pair-bearing bodies), so
/// the `wire + 8` record binding for `1..=24` is a provisional reading
/// awaiting the docs-side I-frame macroblock-layer extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IFrameMbType {
    /// I-frame wire code `0` (dispatch 33) — per-4×4 intra modes +
    /// explicit CBP, luma DCs inline.
    Intra4x4,
    /// I-frame wire codes `1..=24` (dispatch 9..=32) — implied CBP,
    /// separate luma DC block.
    Intra16x16(Intra16x16Params),
    /// I-frame wire code `25` (dispatch 8) — the wiki list's
    /// "luma DCs coded in a separate 4×4 block and no other blocks
    /// coded" type. Assigned by elimination; its exact element
    /// grammar is not pinned by the staged docs.
    SeparateDcOnly,
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
        0 => Ok(IFrameMbType::Intra4x4),
        1..=24 => Ok(IFrameMbType::Intra16x16(
            match Intra16x16Params::from_unified_type(code + 8) {
                Some(p) => p,
                // code + 8 is always in 9..=32 here.
                None => return Err(Error::InvalidFrameCode(code)),
            },
        )),
        25 => Ok(IFrameMbType::SeparateDcOnly),
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

/// Read one universal-code codeword from the bit-reader and classify
/// it against the supplied slice frame type. Equivalent to calling
/// [`read_universal_code`] followed by [`classify_mb_type`].
///
/// Returns [`Error::InvalidFrameCode`] when the decoded code value
/// is outside the per-slice valid range and [`Error::Truncated`]
/// when the bit-reader runs out of bits mid-code.
pub fn read_mb_type(br: &mut BitReader<'_>, frame_type: Svq3FrameType) -> Result<Svq3MbType> {
    let code = read_universal_code(br)?;
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

/// Classification of the neighbour macroblock / sub-block whose
/// previously-decoded intra-prediction mode the current 4×4 sub-block's
/// predictor lookup depends on.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra macroblock
/// information decoding" the per-sub-block predictor lookup
/// `pred_table[top + 1][left + 1][idx]` is fed by the previously-decoded
/// top and left neighbours of the current 4×4 sub-block, with two
/// substitution rules baked into the surrounding prose:
///
/// 1. **"When predictors lie outside of slice, -1 is used instead."** —
///    the neighbour is unavailable because the sub-block sits on the
///    top edge / left edge of the slice. The lookup index becomes
///    `-1 + 1 = 0` (the first row / column of the table).
/// 2. **"For 16×16 intra and any inter blocks value of 2 is used as the
///    predictor."** — the neighbour exists but is not a 4×4 intra-coded
///    sub-block. The lookup index becomes `2 + 1 = 3`.
///
/// Both substitutions are applied independently per neighbour (so the
/// top can be `Outside` while the left is `Intra16x16OrInter`, for
/// example).
///
/// Decoders that have a 4×4-coded neighbour pass [`Self::Mode4x4`] with
/// the neighbour's previously-decoded prediction mode in `0..=4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntraNeighbour {
    /// Neighbour sub-block lies outside the current slice (top edge /
    /// left edge of the slice). Lookup index is `0` (`-1 + 1`).
    Outside,
    /// Neighbour macroblock is a 16×16 intra-coded MB or any inter
    /// macroblock. Lookup index is `3` (`2 + 1`).
    Intra16x16OrInter,
    /// Neighbour is a 4×4 intra-coded sub-block with the carried
    /// prediction mode (one of `0..=4`). Lookup index is `mode + 1`.
    Mode4x4(u8),
}

impl IntraNeighbour {
    /// Number of valid 4×4 intra-prediction mode values per the wiki
    /// spec's `INTRA_PRED_TABLE` shape — the lookup's second axis
    /// spans `0..=5` (`-1..=4` after the spec's `+ 1` adjustment).
    pub const NUM_MODES: u8 = 5;

    /// Convert this neighbour classification to the `0..=5` lookup
    /// index per the wiki spec's `top + 1` / `left + 1` adjustment.
    ///
    /// Returns [`Error::BadBitWidth`] (re-used as a generic
    /// argument-domain error since the SVQ3 4×4 intra-prediction mode
    /// space is `0..=4`) when [`IntraNeighbour::Mode4x4`] carries an
    /// out-of-range mode value (`> 4`). The error preserves the
    /// rejected mode value so the caller can surface a diagnostic.
    pub fn lookup_index(self) -> Result<u8> {
        match self {
            Self::Outside => Ok(0),
            Self::Mode4x4(mode) if mode < Self::NUM_MODES => Ok(mode + 1),
            Self::Mode4x4(mode) => Err(Error::BadBitWidth(u32::from(mode))),
            Self::Intra16x16OrInter => Ok(3),
        }
    }
}

/// Resolve a 4×4 intra-prediction mode from the per-sub-block predictor
/// table given the two neighbour classifications and the per-pair
/// candidate index `idx`.
///
/// Implements the per-sub-block predictor lookup documented in
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra macroblock
/// information decoding":
///
/// > "Each element of the pair is then used as an index in the
/// > prediction table (the proper order is
/// > `pred_table[top + 1][left + 1][idx]`). When predictors lie outside
/// > of slice, -1 is used instead. For 16x16 intra and any inter blocks
/// > value of 2 is used as the predictor. If table value is -1 then
/// > input data was incorrect or intra modes were predicted
/// > incorrectly."
///
/// The `idx` argument is one of the two elements of the
/// [`INTRA_PRED_PAIRS`] entry the slice's intra-mode VLC selected.
/// Pairs are `(u8, u8)` with each element in `0..=4`; each element is
/// independently resolved by a separate call to this helper.
///
/// Returns the resolved intra-prediction mode (one of `0..=4`) on
/// success. Returns [`Error::InvalidIntraPrediction`] when the table
/// entry is the `-1` sentinel — the spec defines this as a malformed
/// bitstream / mispredicted intra-mode condition. Returns
/// [`Error::BadBitWidth`] when `idx > 4` (the table's third axis is
/// `0..=4`).
pub fn resolve_intra_4x4_predictor(
    top: IntraNeighbour,
    left: IntraNeighbour,
    idx: u8,
) -> Result<u8> {
    if idx >= IntraNeighbour::NUM_MODES {
        return Err(Error::BadBitWidth(u32::from(idx)));
    }
    let t = top.lookup_index()?;
    let l = left.lookup_index()?;
    let raw = INTRA_PRED_TABLE[t as usize][l as usize][idx as usize];
    if raw < 0 {
        return Err(Error::InvalidIntraPrediction(t, l, idx));
    }
    // raw is one of 0..=4 per spec.
    Ok(raw as u8)
}

/// Resolve both elements of an [`INTRA_PRED_PAIRS`] entry against the
/// same neighbour pair, returning the `(top_mode, left_mode)` tuple the
/// caller can feed into the per-sub-block intra-prediction stage.
///
/// The wiki spec describes the pair as "Each element of the pair is
/// then used as an index in the prediction table" — both elements are
/// resolved against the **same** `(top_neighbour, left_neighbour)`
/// context but with different `idx` values. The pair's first element
/// resolves the top intra-prediction mode for the current 4×4 sub-block
/// and the second resolves the left intra-prediction mode.
///
/// Returns the resolved `(top_mode, left_mode)` tuple on success.
/// Errors propagate from [`resolve_intra_4x4_predictor`].
pub fn resolve_intra_4x4_pair(
    top: IntraNeighbour,
    left: IntraNeighbour,
    pair: (u8, u8),
) -> Result<(u8, u8)> {
    let top_mode = resolve_intra_4x4_predictor(top, left, pair.0)?;
    let left_mode = resolve_intra_4x4_predictor(top, left, pair.1)?;
    Ok((top_mode, left_mode))
}

/// Per-macroblock motion-vector sample-grid precision used by an
/// SVQ3 inter macroblock.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Inter macroblock
/// information decoding" a P-frame inter macroblock can select between
/// three sample-grid precisions for its motion vectors. The same
/// section also notes (§"Macroblock transform and dequantization",
/// paragraph beginning "Since P-frame macroblocks can have different
/// motion vector precision") that B-frame inter macroblocks always
/// use [`Halfpel`] precision and consume no bit to indicate it.
///
/// [`Halfpel`]: Svq3MvPrecision::Halfpel
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq3MvPrecision {
    /// Whole-pixel grid. The motion-vector components are interpreted
    /// directly as integer sample offsets with no sub-pixel
    /// interpolation.
    Fullpel,
    /// Half-pixel grid. The motion-vector components are interpreted
    /// at the 1/2-sample grid; a sub-pixel interpolation stage is
    /// required when the reference is fetched.
    Halfpel,
    /// Third-pixel grid. The motion-vector components are interpreted
    /// at the 1/3-sample grid; the wiki spec describes this as
    /// "motion compensation may be performed with full-pixel, halfpel
    /// or thirdpel precision" in §"Macroblock layer".
    Thirdpel,
}

/// Read the inter-macroblock motion-vector precision selector for a
/// P-frame inter macroblock.
///
/// Implements the three-branch decision documented in
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Inter macroblock
/// information decoding":
///
/// ```text
///   if has_thirdpel && get_bit() != has_halfpel {
///       use thirdpel mode
///   } else if has_halfpel && get_bit() != has_thirdpel {
///       use halfpel mode
///   } else {
///       use fullpel mode
///   }
/// ```
///
/// `has_thirdpel` / `has_halfpel` come from the sequence header's
/// `has_thirdpel` / `has_halfpel` flags (the two single-bit fields the
/// SEQH parser captures into [`crate::svq3::Svq3SequenceHeader`]).
///
/// Returns the chosen [`Svq3MvPrecision`] on success; returns
/// [`Error::Truncated`] if the bit-reader runs out of input mid-branch.
///
/// Bit consumption follows the spec's short-circuit evaluation
/// exactly:
///
/// | `has_thirdpel` | `has_halfpel` | bits read |
/// | -------------- | ------------- | --------- |
/// | `false`        | `false`       | 0         |
/// | `false`        | `true`        | 1         |
/// | `true`         | `false`       | 1         |
/// | `true`         | `true`        | 1 or 2    |
///
/// (When both flags are set the first branch reads one bit; if that
/// branch is not taken the second branch reads one further bit, for a
/// total of up to two bits.)
pub fn read_inter_mv_precision_p_frame(
    br: &mut BitReader<'_>,
    has_thirdpel: bool,
    has_halfpel: bool,
) -> Result<Svq3MvPrecision> {
    // First branch: `has_thirdpel && get_bit() != has_halfpel` →
    // Thirdpel.
    if has_thirdpel {
        let bit = br.read_bit()? != 0;
        if bit != has_halfpel {
            return Ok(Svq3MvPrecision::Thirdpel);
        }
    }
    // Second branch: `has_halfpel && get_bit() != has_thirdpel` →
    // Halfpel.
    if has_halfpel {
        let bit = br.read_bit()? != 0;
        if bit != has_thirdpel {
            return Ok(Svq3MvPrecision::Halfpel);
        }
    }
    // Else branch: fullpel.
    Ok(Svq3MvPrecision::Fullpel)
}

/// Read the inter-macroblock motion-vector precision selector,
/// dispatching by the enclosing slice's frame type.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
/// transform and dequantization" paragraph beginning "Since P-frame
/// macroblocks can have different motion vector precision", B-frame
/// inter macroblocks always use [`Svq3MvPrecision::Halfpel`] and
/// consume **no** bit to indicate it. P-frame inter macroblocks defer
/// to [`read_inter_mv_precision_p_frame`].
///
/// I-frame inter macroblocks do not exist — the wiki spec's I-frame
/// macroblock type space (§"Macroblock layer") contains only intra
/// codes (`0..=25`). Calling this with [`Svq3FrameType::Intra`]
/// returns the sequence-header-implied fullpel default, but in
/// practice the caller will never invoke this on an I-frame slice.
///
/// Returns [`Error::Truncated`] only on the P-frame branch where bits
/// are consumed.
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

/// The eight `(first_block, second_block)` 4×4 sub-block index pairs
/// that share one intra-mode VLC codeword.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra macroblock
/// information decoding" the 4×4 intra prediction modes are decoded in
/// the order
///
/// ```text
///   ( 0,  1)  ( 4,  5)
///   ( 2,  3)  ( 6,  7)
///   ( 8,  9)  (12, 13)
///   (10, 11)  (14, 15)
/// ```
///
/// where the parenthesised groupings are exactly the
/// `( first_block_index, second_block_index )` pairs read together:
/// "Prediction is performed by reading a variable-length code which
/// corresponds to one of the following pairs" — one VLC codeword
/// resolves the two `(idx_a, idx_b)` table indices of the two blocks of
/// the pair. The eight pairs are taken row-major from the picture:
/// `(0,1) (4,5) (2,3) (6,7) (8,9) (12,13) (10,11) (14,15)`.
///
/// [`INTRA_4X4_SCAN_ORDER`] is the flattened single-block view of this
/// same picture; this table is the paired view the VLC-driven decode
/// consumes (one codeword per pair).
pub const INTRA_4X4_PRED_BLOCK_PAIRS: [(u8, u8); 8] = [
    (0, 1),
    (4, 5),
    (2, 3),
    (6, 7),
    (8, 9),
    (12, 13),
    (10, 11),
    (14, 15),
];

/// Number of `INTRA_PRED_PAIRS` entries the intra-mode VLC selects
/// between (`0..=24`). The wiki §"Intra macroblock information
/// decoding" lists exactly 25 pairs.
pub const INTRA_PRED_PAIRS_LEN: u32 = INTRA_PRED_PAIRS.len() as u32;

/// Read one intra-4×4 prediction-mode VLC codeword and resolve it to
/// its `(idx_a, idx_b)` table-index pair.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Decoding Process"
/// the codec "extensively uses Golomb coding", and §"Intra macroblock
/// information decoding" states the intra-mode pair is selected by
/// "reading a variable-length code which corresponds to one of the
/// following pairs". The 25 pairs are listed in a single contiguous
/// `0..=24` enumeration (`{0,0}; {1,0},{0,1}; {0,2},{1,1},{2,0}; …;
/// {4,4}`), i.e. the listing order is the code-number ordering of the
/// universal code
/// (`docs/video/svq3/spec/06-residual-coefficient-coding.md` §1). This
/// is the same code-number-indexed-listing convention the
/// macroblock-type code uses ([`read_mb_type`] reads one universal
/// codeword then indexes the per-frame MB-type enumeration). The
/// decoded code number indexes [`INTRA_PRED_PAIRS`] directly.
///
/// Returns the `(idx_a, idx_b)` pair on success. Returns
/// [`Error::InvalidFrameCode`] when the decoded code number is `>= 25`
/// (outside the 25-pair alphabet) and propagates [`Error::Truncated`]
/// from the underlying bit-reader.
pub fn read_intra_4x4_pred_pair(br: &mut BitReader<'_>) -> Result<(u8, u8)> {
    let code = read_universal_code(br)?;
    if code >= INTRA_PRED_PAIRS_LEN {
        return Err(Error::InvalidFrameCode(code));
    }
    Ok(INTRA_PRED_PAIRS[code as usize])
}

/// The 16 per-sub-block resolved intra-prediction modes of one
/// 4×4-intra macroblock, in **block-index** order (`modes[index]` is
/// the resolved mode of the 4×4 sub-block at raster index `index`,
/// `0..=15`, laid out row-major within the 16×16 macroblock).
///
/// Each entry is one of `0..=4` (the SVQ3 5-mode intra-4×4 subset, per
/// `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 3). This
/// is the per-macroblock output of [`decode_intra_4x4_modes`]: the
/// sequence of modes feeding the per-sub-block intra predictors in
/// [`crate::svq3_recon`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intra4x4ModeGrid {
    modes: [u8; 16],
}

impl Intra4x4ModeGrid {
    /// The resolved intra-prediction mode (`0..=4`) of the 4×4 sub-block
    /// at raster index `index` (`0..=15`, row-major within the 16×16
    /// macroblock). Returns `None` for `index >= 16`.
    pub fn mode(&self, index: usize) -> Option<u8> {
        self.modes.get(index).copied()
    }

    /// Borrow the full 16-entry block-index-ordered mode array.
    pub fn modes(&self) -> &[u8; 16] {
        &self.modes
    }
}

/// Decode the 16 intra-prediction modes of one 4×4-intra macroblock
/// from the slice bitstream.
///
/// Composes the intra-mode VLC read ([`read_intra_4x4_pred_pair`]) with
/// the per-block predictor table lookup ([`resolve_intra_4x4_predictor`])
/// over the pair-grouped processing order
/// ([`INTRA_4X4_PRED_BLOCK_PAIRS`]) the wiki §"Intra macroblock
/// information decoding" pins. For each of the eight block pairs:
///
/// 1. read one intra-mode VLC codeword → `(idx_a, idx_b)`;
/// 2. resolve block `a`'s mode = `pred_table[top_a+1][left_a+1][idx_a]`
///    against `a`'s already-decoded top / left neighbour modes;
/// 3. resolve block `b`'s mode likewise against `b`'s neighbours
///    (which may include block `a` if `b` is `a`'s right / lower
///    neighbour, since the scan order decodes `a` first).
///
/// `top_avail` / `left_avail` say whether a neighbour macroblock exists
/// above / to the left in the slice (governing the edge sub-blocks'
/// out-of-MB neighbour availability).
///
/// Returns the populated [`Intra4x4ModeGrid`] (modes in block-index
/// order). Propagates [`Error::Truncated`] from the bit-reader,
/// [`Error::InvalidFrameCode`] for an out-of-alphabet VLC code, and
/// [`Error::InvalidIntraPrediction`] when a `pred_table` lookup lands
/// on the `-1` sentinel (a malformed / mispredicted intra stream per
/// the wiki spec).
pub fn decode_intra_4x4_modes(
    br: &mut BitReader<'_>,
    top_avail: bool,
    left_avail: bool,
) -> Result<Intra4x4ModeGrid> {
    let edge = |avail: bool| {
        if avail {
            [IntraNeighbour::Intra16x16OrInter; 4]
        } else {
            [IntraNeighbour::Outside; 4]
        }
    };
    decode_intra_4x4_modes_with_context(br, edge(top_avail), edge(left_avail))
}

/// Decode the 16 intra-prediction modes of one 4×4-intra macroblock
/// with **explicit out-of-macroblock neighbour contexts**.
///
/// The generalised form of [`decode_intra_4x4_modes`]: `top_ctx[c]` is
/// the neighbour classification for the sub-block directly above the
/// top-row block in column `c` (i.e. the bottom-row block of the
/// macroblock above), and `left_ctx[r]` the classification for the
/// sub-block to the left of the left-column block in row `r` (the
/// rightmost-column block of the macroblock to the left). A slice-level
/// frame walk threads the neighbouring macroblocks' decoded 4×4 modes
/// through [`IntraNeighbour::Mode4x4`] — the wiki §"Intra macroblock
/// information decoding" `pred_table[top + 1][left + 1][idx]` context
/// substitutes the *actual* neighbour mode when the neighbour
/// macroblock is itself 4×4-intra-coded, `2` when it is 16×16-intra or
/// inter ([`IntraNeighbour::Intra16x16OrInter`]), and `-1` outside the
/// slice ([`IntraNeighbour::Outside`]).
///
/// In-macroblock neighbour sequencing follows the pair scan order
/// exactly as in [`decode_intra_4x4_modes`]; errors propagate
/// identically.
pub fn decode_intra_4x4_modes_with_context(
    br: &mut BitReader<'_>,
    top_ctx: [IntraNeighbour; 4],
    left_ctx: [IntraNeighbour; 4],
) -> Result<Intra4x4ModeGrid> {
    let mut decoded: [Option<u8>; 16] = [None; 16];

    let neighbours = |index: usize, decoded: &[Option<u8>; 16]| {
        let row = index / 4;
        let col = index % 4;
        let top = if row == 0 {
            top_ctx[col]
        } else {
            match decoded[index - 4] {
                Some(m) => IntraNeighbour::Mode4x4(m),
                None => IntraNeighbour::Outside,
            }
        };
        let left = if col == 0 {
            left_ctx[row]
        } else {
            match decoded[index - 1] {
                Some(m) => IntraNeighbour::Mode4x4(m),
                None => IntraNeighbour::Outside,
            }
        };
        (top, left)
    };

    for &(block_a, block_b) in INTRA_4X4_PRED_BLOCK_PAIRS.iter() {
        let (idx_a, idx_b) = read_intra_4x4_pred_pair(br)?;

        let ia = block_a as usize;
        let (top_a, left_a) = neighbours(ia, &decoded);
        let mode_a = resolve_intra_4x4_predictor(top_a, left_a, idx_a)?;
        decoded[ia] = Some(mode_a);

        let ib = block_b as usize;
        // Block `b`'s neighbours are resolved AFTER `a` is recorded, so
        // if `b` is below / right of `a` it sees `a`'s mode.
        let (top_b, left_b) = neighbours(ib, &decoded);
        let mode_b = resolve_intra_4x4_predictor(top_b, left_b, idx_b)?;
        decoded[ib] = Some(mode_b);
    }

    // Every block is decoded exactly once (the eight pairs cover all 16
    // indices); unwrap the running options into the final grid.
    let mut modes = [0u8; 16];
    for (i, slot) in decoded.iter().enumerate() {
        // SAFETY of the unwrap: INTRA_4X4_PRED_BLOCK_PAIRS is a
        // permutation of 0..16 (asserted in tests), so each slot is set.
        modes[i] = slot.expect("every 4x4 block index is covered by the pair table");
    }
    Ok(Intra4x4ModeGrid { modes })
}

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

    /// Universal-code encoding helper (spec/06 §1 interleaved layout) —
    /// produces a `(width, value)` pack item that decodes to `n`.
    ///
    /// `ue(0) = "1"`, `ue(1) = "010"`, `ue(2) = "011"`,
    /// `ue(3) = "00001"`, `ue(7) = "0000001"`, … — codes 0..=2 match
    /// the familiar exp-Golomb layout, higher codes interleave
    /// terminator bits among the data bits.
    fn ue(n: u32) -> (u32, u32) {
        let exp = 31 - (n + 1).leading_zeros();
        let data = n + 1 - (1u32 << exp);
        match exp {
            0 => (1, 1),
            1 => (3, 0b010 | data),
            _ => {
                let mut bits: u32 = 0b00;
                bits = (bits << 1) | ((data >> (exp - 1)) & 1);
                bits = (bits << 1) | ((data >> (exp - 2)) & 1);
                let mut width = 4;
                for i in (0..exp - 2).rev() {
                    bits = (bits << 2) | ((data >> i) & 1);
                    width += 2;
                }
                (width + 1, (bits << 1) | 1)
            }
        }
    }

    #[test]
    fn i_frame_code_table() {
        // Code 0 → intra 4×4 (dispatch 33) — pinned by the black-frame
        // structural census of the staged 320×240 fixture (see the
        // IFrameMbType docs).
        assert_eq!(
            classify_mb_type(Svq3FrameType::Intra, 0).unwrap(),
            Svq3MbType::IIntra(IFrameMbType::Intra4x4)
        );
        // Codes 1..=24 → intra 16×16 with the tables/03 factorisation
        // of dispatch type code + 8.
        for code in 1..=24u32 {
            let got = classify_mb_type(Svq3FrameType::Intra, code).unwrap();
            let params = Intra16x16Params::from_unified_type(code + 8).unwrap();
            assert_eq!(got, Svq3MbType::IIntra(IFrameMbType::Intra16x16(params)));
            assert_eq!(params.unified_type(), code + 8);
        }
        // Code 25 → the separate-DC / no-other-blocks type (dispatch 8,
        // by elimination).
        assert_eq!(
            classify_mb_type(Svq3FrameType::Intra, 25).unwrap(),
            Svq3MbType::IIntra(IFrameMbType::SeparateDcOnly)
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
        // P-frame code 8 ↔ I-frame code 0 = intra 4×4 (wiki −8 rule;
        // the P-side wire↔dispatch correspondence is a docs gap — see
        // the IFrameMbType docs).
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 8).unwrap(),
            Svq3MbType::PIntra(IFrameMbType::Intra4x4)
        );
        // P-frame code 9 = unified 9 = the first intra 16×16 type.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 9).unwrap(),
            Svq3MbType::PIntra(IFrameMbType::Intra16x16(
                Intra16x16Params::from_unified_type(9).unwrap()
            ))
        );
        // P-frame code 32 = unified 32 = the last intra 16×16 type.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 32).unwrap(),
            Svq3MbType::PIntra(IFrameMbType::Intra16x16(
                Intra16x16Params::from_unified_type(32).unwrap()
            ))
        );
        // P-frame code 33 ↔ I-frame code 25 = the separate-DC-only type.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Predicted, 33).unwrap(),
            Svq3MbType::PIntra(IFrameMbType::SeparateDcOnly)
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
        // B-frame code 4 ↔ I-frame code 0 = intra 4×4.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Bidirectional, 4).unwrap(),
            Svq3MbType::BIntra(IFrameMbType::Intra4x4)
        );
        // B-frame code 29 ↔ I-frame code 25 = the separate-DC-only type.
        assert_eq!(
            classify_mb_type(Svq3FrameType::Bidirectional, 29).unwrap(),
            Svq3MbType::BIntra(IFrameMbType::SeparateDcOnly)
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
        let intra = Svq3MbType::IIntra(IFrameMbType::Intra4x4);
        assert!(intra.is_intra());
        assert!(!intra.is_inter());
        assert!(!intra.is_skip());

        let p_intra = Svq3MbType::PIntra(IFrameMbType::Intra4x4);
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
        let sixteen = IFrameMbType::Intra16x16(Intra16x16Params::from_unified_type(16).unwrap());
        let i = Svq3MbType::IIntra(sixteen);
        assert_eq!(i.intra(), Some(sixteen));
        let p_i = Svq3MbType::PIntra(sixteen);
        assert_eq!(p_i.intra(), Some(sixteen));
        let b_i = Svq3MbType::BIntra(sixteen);
        assert_eq!(b_i.intra(), Some(sixteen));
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
        // ue(0) = "1" → code 0 → intra 4×4.
        let bytes = pack(&[(1, 0b1)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Intra).unwrap();
        assert_eq!(mb, Svq3MbType::IIntra(IFrameMbType::Intra4x4));

        // Universal code 25: n = 4, value = 10 = 0b1010 → bits
        // "0 0 1 0 0 1 0 0 1" (0 0 d1 d2 0 d3 0 d4 1) = 0b001001001.
        let (w, v) = ue(25);
        assert_eq!(w, 9);
        assert_eq!(v, 0b001001001);
        let bytes = pack(&[(w, v)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Intra).unwrap();
        assert_eq!(mb, Svq3MbType::IIntra(IFrameMbType::SeparateDcOnly));
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

        // ue(8) → P-intra of I-code 0 = intra 4×4.
        let bytes = pack(&[ue(8)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Predicted).unwrap();
        assert_eq!(mb, Svq3MbType::PIntra(IFrameMbType::Intra4x4));
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

        // ue(4) → B-intra of I-code 0 = intra 4×4.
        let bytes = pack(&[ue(4)]);
        let mut br = BitReader::new(&bytes);
        let mb = read_mb_type(&mut br, Svq3FrameType::Bidirectional).unwrap();
        assert_eq!(mb, Svq3MbType::BIntra(IFrameMbType::Intra4x4));
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

    // ----- inter-MB motion-vector precision selector ---------------

    /// Helper: pack `bits` as a left-aligned byte (MSB-first).
    /// `nbits` is the number of valid bits (1..=8). The remaining
    /// low-order bits are zero-padded so the BitReader's read_bit()
    /// stream returns them as 0.
    fn pack_bits_msb_first(nbits: u32, value: u32) -> Vec<u8> {
        pack(&[(nbits, value)])
    }

    #[test]
    fn precision_no_thirdpel_no_halfpel_is_fullpel_no_bits_read() {
        // has_thirdpel = false, has_halfpel = false.
        // Spec: both branches short-circuit on the && → fullpel,
        // no bit consumed.
        let bytes: [u8; 1] = [0xFF]; // sentinel — must not be read
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision_p_frame(&mut br, false, false).unwrap();
        assert_eq!(p, Svq3MvPrecision::Fullpel);
        assert_eq!(br.bits_consumed(), 0);
    }

    #[test]
    fn precision_only_halfpel_bit1_is_halfpel() {
        // has_thirdpel=false, has_halfpel=true.
        // First branch skipped (has_thirdpel=false). Second branch
        // reads bit; condition `bit != has_thirdpel` → `bit != false`
        // → bit == 1 → Halfpel.
        let bytes = pack_bits_msb_first(1, 1);
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision_p_frame(&mut br, false, true).unwrap();
        assert_eq!(p, Svq3MvPrecision::Halfpel);
        assert_eq!(br.bits_consumed(), 1);
    }

    #[test]
    fn precision_only_halfpel_bit0_is_fullpel() {
        // has_thirdpel=false, has_halfpel=true, bit=0.
        // Second branch: `bit != has_thirdpel` → `0 != 0` → false
        // → falls through to fullpel.
        let bytes = pack_bits_msb_first(1, 0);
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision_p_frame(&mut br, false, true).unwrap();
        assert_eq!(p, Svq3MvPrecision::Fullpel);
        assert_eq!(br.bits_consumed(), 1);
    }

    #[test]
    fn precision_only_thirdpel_bit1_is_thirdpel() {
        // has_thirdpel=true, has_halfpel=false, bit=1.
        // First branch: `bit != has_halfpel` → `1 != 0` → true →
        // Thirdpel.
        let bytes = pack_bits_msb_first(1, 1);
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision_p_frame(&mut br, true, false).unwrap();
        assert_eq!(p, Svq3MvPrecision::Thirdpel);
        assert_eq!(br.bits_consumed(), 1);
    }

    #[test]
    fn precision_only_thirdpel_bit0_is_fullpel() {
        // has_thirdpel=true, has_halfpel=false, bit=0.
        // First branch: `0 != 0` → false → not taken.
        // Second branch: `has_halfpel` is false → not taken.
        // Falls through to fullpel.
        let bytes = pack_bits_msb_first(1, 0);
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision_p_frame(&mut br, true, false).unwrap();
        assert_eq!(p, Svq3MvPrecision::Fullpel);
        assert_eq!(br.bits_consumed(), 1);
    }

    #[test]
    fn precision_both_first_bit0_is_thirdpel() {
        // has_thirdpel=true, has_halfpel=true, first bit=0.
        // First branch: `0 != 1` → true → Thirdpel. Second bit never
        // consumed.
        let bytes = pack_bits_msb_first(2, 0b01); // bit0=0, bit1=1 (unused)
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision_p_frame(&mut br, true, true).unwrap();
        assert_eq!(p, Svq3MvPrecision::Thirdpel);
        assert_eq!(br.bits_consumed(), 1);
    }

    #[test]
    fn precision_both_bits_10_is_halfpel() {
        // has_thirdpel=true, has_halfpel=true.
        // First bit = 1 → `1 != 1` → false → first branch not taken.
        // Second bit = 0 → `0 != 1` → true → Halfpel.
        let bytes = pack_bits_msb_first(2, 0b10);
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision_p_frame(&mut br, true, true).unwrap();
        assert_eq!(p, Svq3MvPrecision::Halfpel);
        assert_eq!(br.bits_consumed(), 2);
    }

    #[test]
    fn precision_both_bits_11_is_fullpel() {
        // has_thirdpel=true, has_halfpel=true.
        // First bit = 1 → `1 != 1` → false → first branch not taken.
        // Second bit = 1 → `1 != 1` → false → second branch not
        // taken. Falls through to fullpel.
        let bytes = pack_bits_msb_first(2, 0b11);
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision_p_frame(&mut br, true, true).unwrap();
        assert_eq!(p, Svq3MvPrecision::Fullpel);
        assert_eq!(br.bits_consumed(), 2);
    }

    #[test]
    fn precision_p_frame_truncated_returns_error() {
        // has_thirdpel=true but no bits available.
        let mut br = BitReader::new(&[]);
        let err = read_inter_mv_precision_p_frame(&mut br, true, false).unwrap_err();
        assert!(matches!(err, Error::Truncated));
    }

    #[test]
    fn precision_p_frame_both_flags_consumes_at_most_two_bits() {
        // has_thirdpel=true, has_halfpel=true: regardless of which
        // sub-branch is taken, the function consumes at most 2 bits.
        // Sweep all four (bit0, bit1) combinations and verify the
        // bit-consumption ceiling.
        for first in 0..=1u32 {
            for second in 0..=1u32 {
                let bytes = pack_bits_msb_first(2, (first << 1) | second);
                let mut br = BitReader::new(&bytes);
                let _ = read_inter_mv_precision_p_frame(&mut br, true, true).unwrap();
                assert!(br.bits_consumed() <= 2);
                // When first=0 (Thirdpel branch taken) only 1 bit
                // consumed.
                if first == 0 {
                    assert_eq!(br.bits_consumed(), 1);
                } else {
                    assert_eq!(br.bits_consumed(), 2);
                }
            }
        }
    }

    #[test]
    fn precision_dispatch_b_frame_always_halfpel_no_bits_read() {
        // B-frame inter macroblocks always halfpel; no bits consumed.
        let bytes: [u8; 1] = [0xAA]; // sentinel — must not be read
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision(&mut br, Svq3FrameType::Bidirectional, true, true).unwrap();
        assert_eq!(p, Svq3MvPrecision::Halfpel);
        assert_eq!(br.bits_consumed(), 0);
    }

    #[test]
    fn precision_dispatch_p_frame_delegates() {
        // P-frame branch matches the standalone function exactly.
        let bytes = pack_bits_msb_first(1, 1);
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision(&mut br, Svq3FrameType::Predicted, true, false).unwrap();
        assert_eq!(p, Svq3MvPrecision::Thirdpel);
        assert_eq!(br.bits_consumed(), 1);
    }

    #[test]
    fn precision_dispatch_i_frame_is_fullpel_no_bits_read() {
        // I-frame slices have no inter macroblocks; the dispatch
        // returns fullpel without reading any bit (defensive
        // behaviour; in practice the caller never reaches here).
        let bytes: [u8; 1] = [0x55];
        let mut br = BitReader::new(&bytes);
        let p = read_inter_mv_precision(&mut br, Svq3FrameType::Intra, true, true).unwrap();
        assert_eq!(p, Svq3MvPrecision::Fullpel);
        assert_eq!(br.bits_consumed(), 0);
    }

    #[test]
    fn precision_enum_variants_distinct() {
        // Sanity — the three variants are distinct.
        assert_ne!(Svq3MvPrecision::Fullpel, Svq3MvPrecision::Halfpel);
        assert_ne!(Svq3MvPrecision::Halfpel, Svq3MvPrecision::Thirdpel);
        assert_ne!(Svq3MvPrecision::Fullpel, Svq3MvPrecision::Thirdpel);
    }

    #[test]
    fn precision_enum_is_copy_and_hashable_for_match_use() {
        // Compile-time check via std::mem::size_of and a match. If
        // the enum ever grows non-Copy fields these stop compiling.
        fn require_copy<T: Copy>() {}
        require_copy::<Svq3MvPrecision>();
        let p = Svq3MvPrecision::Halfpel;
        let q = p;
        assert_eq!(p, q);
    }

    #[test]
    fn neighbour_lookup_index_outside_is_zero() {
        // Per the wiki spec "When predictors lie outside of slice,
        // -1 is used instead" → `-1 + 1 = 0`.
        assert_eq!(IntraNeighbour::Outside.lookup_index().unwrap(), 0);
    }

    #[test]
    fn neighbour_lookup_index_intra16x16_or_inter_is_three() {
        // Per the wiki spec "For 16x16 intra and any inter blocks
        // value of 2 is used as the predictor" → `2 + 1 = 3`.
        assert_eq!(IntraNeighbour::Intra16x16OrInter.lookup_index().unwrap(), 3);
    }

    #[test]
    fn neighbour_lookup_index_mode4x4_offsets_by_one() {
        // Per the wiki spec the table is indexed `top + 1` /
        // `left + 1`. A 4×4-coded neighbour with mode `m` resolves
        // to `m + 1`.
        for m in 0..=4u8 {
            assert_eq!(IntraNeighbour::Mode4x4(m).lookup_index().unwrap(), m + 1);
        }
    }

    #[test]
    fn neighbour_lookup_rejects_out_of_range_mode() {
        // The 4×4 intra-prediction mode space is `0..=4` per the
        // wiki spec; the table's second axis only spans those five
        // entries plus the `-1` sentinel. Anything `> 4` is bad input.
        let err = IntraNeighbour::Mode4x4(5).lookup_index().unwrap_err();
        assert!(matches!(err, Error::BadBitWidth(5)));
        let err = IntraNeighbour::Mode4x4(255).lookup_index().unwrap_err();
        assert!(matches!(err, Error::BadBitWidth(255)));
    }

    #[test]
    fn resolve_intra_4x4_predictor_both_outside_returns_two() {
        // `pred_table[0][0]` per the wiki spec is `[2, -1, -1, -1, -1]`.
        // `idx = 0` resolves to `2`.
        assert_eq!(
            resolve_intra_4x4_predictor(IntraNeighbour::Outside, IntraNeighbour::Outside, 0)
                .unwrap(),
            2
        );
    }

    #[test]
    fn resolve_intra_4x4_predictor_outside_sentinel_idx_errors() {
        // `pred_table[0][0][1..=4]` per the wiki spec is all `-1`. The
        // spec defines this as "input data was incorrect or intra modes
        // were predicted incorrectly" — surface as
        // `InvalidIntraPrediction`.
        for idx in 1..=4u8 {
            let err =
                resolve_intra_4x4_predictor(IntraNeighbour::Outside, IntraNeighbour::Outside, idx)
                    .unwrap_err();
            assert!(matches!(err, Error::InvalidIntraPrediction(0, 0, i) if i == idx));
        }
    }

    #[test]
    fn resolve_intra_4x4_predictor_idx_too_large_errors() {
        // The table's third axis is `0..=4`. `idx >= 5` is bad input.
        let err = resolve_intra_4x4_predictor(IntraNeighbour::Outside, IntraNeighbour::Outside, 5)
            .unwrap_err();
        assert!(matches!(err, Error::BadBitWidth(5)));
    }

    #[test]
    fn resolve_intra_4x4_predictor_matches_table_entry() {
        // Spot-check several `(top, left, idx)` combinations against
        // the spec table. `pred_table[1][1]` is `[0, 2, 1, 4, 3]` so
        // `(Mode4x4(0), Mode4x4(0), 0..=4)` should yield `0, 2, 1, 4, 3`.
        let expected = [0u8, 2, 1, 4, 3];
        for (idx, &want) in expected.iter().enumerate() {
            assert_eq!(
                resolve_intra_4x4_predictor(
                    IntraNeighbour::Mode4x4(0),
                    IntraNeighbour::Mode4x4(0),
                    idx as u8,
                )
                .unwrap(),
                want
            );
        }
        // `pred_table[5][5]` is `[4, 2, 1, 0, 3]`.
        let expected = [4u8, 2, 1, 0, 3];
        for (idx, &want) in expected.iter().enumerate() {
            assert_eq!(
                resolve_intra_4x4_predictor(
                    IntraNeighbour::Mode4x4(4),
                    IntraNeighbour::Mode4x4(4),
                    idx as u8,
                )
                .unwrap(),
                want
            );
        }
    }

    #[test]
    fn resolve_intra_4x4_predictor_intra16x16_neighbour_uses_index_three() {
        // `Intra16x16OrInter` substitutes to lookup index `3`. With
        // `top = Intra16x16OrInter` and `left = Mode4x4(1)` the lookup
        // is `pred_table[3][2]` which is `[1, 2, 0, 4, 3]` per the
        // wiki spec.
        let expected = [1u8, 2, 0, 4, 3];
        for (idx, &want) in expected.iter().enumerate() {
            assert_eq!(
                resolve_intra_4x4_predictor(
                    IntraNeighbour::Intra16x16OrInter,
                    IntraNeighbour::Mode4x4(1),
                    idx as u8,
                )
                .unwrap(),
                want
            );
        }
    }

    #[test]
    fn resolve_intra_4x4_pair_resolves_both_elements() {
        // Pick a pair from `INTRA_PRED_PAIRS` — entry `[4]` is `(1, 1)`,
        // both elements are `1`. With both neighbours `Mode4x4(0)` the
        // lookup is `pred_table[1][1][1] = 2` for both elements.
        let (top_mode, left_mode) = resolve_intra_4x4_pair(
            IntraNeighbour::Mode4x4(0),
            IntraNeighbour::Mode4x4(0),
            (1, 1),
        )
        .unwrap();
        assert_eq!(top_mode, 2);
        assert_eq!(left_mode, 2);

        // Entry `[10]` is `(0, 4)` — `pred_table[1][1][0] = 0` and
        // `pred_table[1][1][4] = 3`.
        let (top_mode, left_mode) = resolve_intra_4x4_pair(
            IntraNeighbour::Mode4x4(0),
            IntraNeighbour::Mode4x4(0),
            (0, 4),
        )
        .unwrap();
        assert_eq!(top_mode, 0);
        assert_eq!(left_mode, 3);
    }

    #[test]
    fn resolve_intra_4x4_pair_propagates_sentinel_error() {
        // `pred_table[0][0][1] = -1`. Resolving the pair `(0, 1)` with
        // both neighbours outside should error on the second element
        // (the first element `idx = 0` resolves to `2`; the second
        // `idx = 1` hits the `-1` sentinel).
        let err = resolve_intra_4x4_pair(IntraNeighbour::Outside, IntraNeighbour::Outside, (0, 1))
            .unwrap_err();
        assert!(matches!(err, Error::InvalidIntraPrediction(0, 0, 1)));
    }

    #[test]
    fn resolve_intra_4x4_predictor_covers_all_intra_pred_pairs_for_intra_neighbours() {
        // Every entry in `INTRA_PRED_PAIRS` has both elements in
        // `0..=4` per the wiki spec. With both neighbours encoded as
        // 4×4 intra MBs the lookup must succeed for both elements of
        // every one of the 25 pairs.
        for &(a, b) in INTRA_PRED_PAIRS.iter() {
            for top_mode in 0..=4u8 {
                for left_mode in 0..=4u8 {
                    let top = IntraNeighbour::Mode4x4(top_mode);
                    let left = IntraNeighbour::Mode4x4(left_mode);
                    // Both elements must yield modes in `0..=4` since
                    // `pred_table[top + 1][left + 1]` for `top, left
                    // ∈ 0..=4` (i.e. `[1..=5][1..=5]`) has no `-1`
                    // entries per the wiki spec table.
                    let resolved_a = resolve_intra_4x4_predictor(top, left, a).unwrap();
                    let resolved_b = resolve_intra_4x4_predictor(top, left, b).unwrap();
                    assert!(resolved_a < 5);
                    assert!(resolved_b < 5);
                }
            }
        }
    }

    #[test]
    fn intra_4x4_pred_block_pairs_is_permutation_of_0_to_15() {
        // The eight pairs must cover every 4×4 sub-block index 0..16
        // exactly once (one VLC codeword per pair, 8 pairs × 2 = 16).
        let mut seen = [false; 16];
        for &(a, b) in INTRA_4X4_PRED_BLOCK_PAIRS.iter() {
            for v in [a, b] {
                assert!((v as usize) < 16);
                assert!(!seen[v as usize], "block {v} covered twice");
                seen[v as usize] = true;
            }
        }
        assert!(seen.iter().all(|&s| s), "not every block index covered");
        assert_eq!(INTRA_4X4_PRED_BLOCK_PAIRS.len(), 8);
    }

    #[test]
    fn intra_4x4_pred_block_pairs_match_scan_order_grouping() {
        // The paired view must equal the flat scan order taken two at a
        // time: scan = [0,1, 4,5, 2,3, 6,7, 8,9, 12,13, 10,11, 14,15].
        for (pair_idx, &(a, b)) in INTRA_4X4_PRED_BLOCK_PAIRS.iter().enumerate() {
            assert_eq!(a, INTRA_4X4_SCAN_ORDER[pair_idx * 2]);
            assert_eq!(b, INTRA_4X4_SCAN_ORDER[pair_idx * 2 + 1]);
        }
    }

    #[test]
    fn intra_pred_pairs_len_constant() {
        assert_eq!(INTRA_PRED_PAIRS_LEN, 25);
        assert_eq!(INTRA_PRED_PAIRS_LEN as usize, INTRA_PRED_PAIRS.len());
    }

    #[test]
    fn read_intra_4x4_pred_pair_indexes_pairs_table() {
        // ue(code) for each of the 25 valid codes must decode to the
        // matching INTRA_PRED_PAIRS entry.
        for code in 0..25u32 {
            let bytes = pack(&[ue(code)]);
            let mut br = BitReader::new(&bytes);
            let got = read_intra_4x4_pred_pair(&mut br).unwrap();
            assert_eq!(got, INTRA_PRED_PAIRS[code as usize], "code {code}");
        }
    }

    #[test]
    fn read_intra_4x4_pred_pair_rejects_out_of_alphabet_code() {
        // code 25 is just past the 25-pair alphabet (0..=24).
        let bytes = pack(&[ue(25)]);
        let mut br = BitReader::new(&bytes);
        let err = read_intra_4x4_pred_pair(&mut br).unwrap_err();
        assert!(matches!(err, Error::InvalidFrameCode(25)));
    }

    #[test]
    fn read_intra_4x4_pred_pair_truncated() {
        // Empty input → Truncated on the first leading-zero read.
        let bytes: [u8; 0] = [];
        let mut br = BitReader::new(&bytes);
        assert!(matches!(
            read_intra_4x4_pred_pair(&mut br),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn read_intra_4x4_pred_pair_consumes_exact_bits() {
        // Two back-to-back codes must decode independently with no
        // bit-slip between them. code 0 = single '1' bit, code 3 has
        // ue width 5.
        let bytes = pack(&[ue(0), ue(3)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(read_intra_4x4_pred_pair(&mut br).unwrap(), (0, 0));
        assert_eq!(read_intra_4x4_pred_pair(&mut br).unwrap(), (0, 2));
    }

    #[test]
    fn decode_intra_4x4_modes_all_code_zero_top_left_unavailable() {
        // Eight codeword-0 reads → every pair is (0, 0), i.e. idx_a =
        // idx_b = 0. With no neighbour MBs available, every edge
        // neighbour is Outside. pred_table[0][0][0] = 2 and once a
        // block decodes to mode 2 the in-MB neighbours become
        // Mode4x4(2): pred_table[3][?]/[?][3] still resolves at idx 0.
        // We just assert the call succeeds and yields modes in 0..=4.
        let bytes = pack(&[ue(0); 8]);
        let mut br = BitReader::new(&bytes);
        let grid = decode_intra_4x4_modes(&mut br, false, false).unwrap();
        for &m in grid.modes() {
            assert!(m < 5);
        }
        // Block 0 sits at the top-left corner: both neighbours Outside,
        // idx 0 → pred_table[0][0][0] = 2.
        assert_eq!(grid.mode(0), Some(2));
    }

    #[test]
    fn decode_intra_4x4_modes_threads_in_mb_neighbours() {
        // Verify that block 1 (top-right of the first pair) sees block
        // 0's decoded mode as its LEFT neighbour. With top/left MB
        // unavailable: block 0 = pred_table[0][0][idx_a]; block 1's
        // left neighbour is block 0 (col 1, row 0), its top neighbour
        // is Outside (row 0). So block 1 = pred_table[0][mode0+1][idx_b].
        //
        // Use the first pair code = 4 → INTRA_PRED_PAIRS[4] = (1,1):
        // idx_a = idx_b = 1.
        //   block 0: pred_table[0][0][1] = -1 → would error. Use code 1
        //   instead: INTRA_PRED_PAIRS[1] = (1, 0).
        //   block 0: top=Outside(0) left=Outside(0) idx=1 →
        //            pred_table[0][0][1] = -1 → error. So pick a pair
        //   whose first idx is 0. code 0 = (0,0): block0 idx 0 →
        //   pred_table[0][0][0] = 2. block1: top=Outside left=Mode4x4(2)
        //   idx 0 → pred_table[0][3][0]. INTRA_PRED_TABLE[0][3] =
        //   [2,1,-1,-1,-1] → idx 0 = 2.
        let bytes = pack(&[ue(0); 8]);
        let mut br = BitReader::new(&bytes);
        let grid = decode_intra_4x4_modes(&mut br, false, false).unwrap();
        // Block 1: top = Outside (row 0), left = block 0 = Mode4x4(2).
        // pred_table[top+1=0][left+1=3][idx=0] = INTRA_PRED_TABLE[0][3][0] = 2.
        assert_eq!(grid.mode(1), Some(2));
        // Block 2 sits at row 1, col 0: top = block 0 (index 2-4 ... wait
        // index 2 is row 0 col 2). Use the grid contract: index/4 = row,
        // index%4 = col. index 4 = row 1 col 0; its top neighbour is
        // index 0. Verify index 4's top is block 0's mode (2):
        // pred_table[mode0+1=3][left=Outside+1=0][idx]. For the second
        // pair (4,5) code 0 → idx_a=idx_b=0. INTRA_PRED_TABLE[3][0][0] = 2.
        assert_eq!(grid.mode(4), Some(2));
    }

    #[test]
    fn decode_intra_4x4_modes_propagates_sentinel_error() {
        // First pair code = 1 → INTRA_PRED_PAIRS[1] = (1, 0). Block 0
        // top/left Outside, idx_a = 1 → pred_table[0][0][1] = -1 →
        // InvalidIntraPrediction.
        let bytes = pack(&[ue(1)]);
        let mut br = BitReader::new(&bytes);
        let err = decode_intra_4x4_modes(&mut br, false, false).unwrap_err();
        assert!(matches!(err, Error::InvalidIntraPrediction(0, 0, 1)));
    }

    #[test]
    fn decode_intra_4x4_modes_truncated_midway() {
        // Only one valid codeword present (code 0); the loop needs 8.
        let bytes = pack(&[ue(0)]);
        let mut br = BitReader::new(&bytes);
        assert!(matches!(
            decode_intra_4x4_modes(&mut br, false, false),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn decode_intra_4x4_modes_with_available_neighbour_mbs() {
        // With top/left MBs available, edge neighbours are
        // Intra16x16OrInter (index 3). Block 0: top=3, left=3, idx 0 →
        // pred_table[3][3][0]. INTRA_PRED_TABLE[3][3] = [2,1,0,4,3] →
        // idx 0 = 2. Decode must still succeed and yield 0..=4 modes.
        let bytes = pack(&[ue(0); 8]);
        let mut br = BitReader::new(&bytes);
        let grid = decode_intra_4x4_modes(&mut br, true, true).unwrap();
        assert_eq!(grid.mode(0), Some(2));
        for &m in grid.modes() {
            assert!(m < 5);
        }
    }

    #[test]
    fn intra_4x4_mode_grid_accessors() {
        let bytes = pack(&[ue(0); 8]);
        let mut br = BitReader::new(&bytes);
        let grid = decode_intra_4x4_modes(&mut br, false, false).unwrap();
        assert_eq!(grid.mode(16), None);
        assert_eq!(grid.modes().len(), 16);
        // mode() and modes() must agree for every in-range index.
        for i in 0..16 {
            assert_eq!(grid.mode(i), Some(grid.modes()[i]));
        }
    }
}
