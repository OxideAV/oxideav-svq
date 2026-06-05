//! SVQ1 mean-removal application: the saturating add step that
//! biases every leaf sample by the per-leaf mean before any
//! per-stage codebook residue is summed.
//!
//! ## Scope
//!
//! Round 239 lands the **mean-step arithmetic** documented in
//! `docs/video/svq1/spec/05-mean-removal.md` §5.4 — the saturating
//! per-sample add of the per-leaf mean against the per-sample
//! predictor — for both the intra path (predictor = `0`, mean ∈
//! `[0, 255]`) and the inter path (predictor ∈ `[0, 255]`, mean ∈
//! `[-256, +255]`). The chapter is HEADER-FACING for the field
//! ranges (per §5.1) and OPERATIONAL for the arithmetic (per §5.4.2
//! and §5.4.3 — "per-stage saturation also applies to the mean
//! step").
//!
//! What this module pins per spec/05:
//!
//! * The intra mean range — `[0, 255]` (per §5.1 / §5.1.1). The
//!   intra path's predictor is unconditionally `0` per §5.4.1.
//! * The inter mean range — `[-256, +255]` (per §5.1 / §5.1.2 /
//!   §5.1.3 "Why 9 bits, not 8"). The inter path's predictor is the
//!   motion-compensated reference sample ∈ `[0, 255]` per §5.4.
//! * The saturating add itself — `saturate_u8(predictor + mean)`
//!   per §5.4 the per-sample formula and §5.4.3 "the per-stage
//!   clamp applies to predictor + mean before any codebook stage is
//!   added". The clamp is to `[0, 255]` (unsigned char) per §5
//!   throughout.
//!
//! What this module does **not** cover:
//!
//! * Reading the mean VLC from the bitstream — that is the role of
//!   a future Extractor + Specifier round (per §5.7 the mean-VLC
//!   tables are pinned at file offsets `0x5cb0c..0x5cf14` (intra,
//!   alphabet 256) and `0x5c304..0x5cb0c` (inter, alphabet 512 +
//!   `min_value = -256`) but the codeword bit-patterns are not yet
//!   wired into this crate's source).
//! * The block-replication step (per §5.4 the mean is replicated
//!   across all `V_L` sample positions). This module produces the
//!   **per-sample** mean step; the caller iterates over the leaf's
//!   sample positions.
//! * Stage-count VLC decoding (the `N` value that gates whether
//!   the mean is read at all per §5.5). The mean-step arithmetic
//!   here is applied unconditionally — the caller decides whether
//!   to invoke it.
//! * SKIP-mode short-circuit (per §5.5 — `N = -1` skips the mean
//!   read entirely; the block stays unchanged). The mean step here
//!   has no SKIP path.
//!
//! ## Mean-value typing
//!
//! Per §5.1.1 / §5.1.2 the two halves use different signed-ness
//! domains, so this module exposes two `const fn` entry points:
//!
//! * [`apply_intra_mean_step`] — the intra path. Takes the mean as
//!   a `u8` (the natural domain for `[0, 255]`) and the predictor
//!   as a `u8`. Returns the post-clamp `u8` value. For the intra
//!   path proper, the predictor argument is unconditionally `0`
//!   per §5.4.1; the helper accepts any predictor so the SAME
//!   arithmetic can serve the INTRA-mode P/B-frame leaves
//!   documented in §5.6's "MB-level coding" table (those leaves
//!   also use the intra mean table per §5.6's row "P/B-frame
//!   INTRA → intra mean table").
//! * [`apply_inter_mean_step`] — the inter path. Takes the mean as
//!   an `i16` in `[-256, +255]` (a `u8` cannot hold the negative
//!   half; an `i8` cannot hold `+255`) and the predictor as a `u8`
//!   (the motion-compensated reference sample). Returns the
//!   post-clamp `u8` value. The helper also validates the mean's
//!   range — values outside `[-256, +255]` return
//!   [`MeanError::OutOfRange`].
//!
//! The boundary values are exercised by the unit tests at the
//! bottom of the module.
//!
//! ## Relationship to the predictor
//!
//! Per §5.4 the predictor's domain is `[0, 255]` for both halves
//! (it is either zero or a motion-compensated `u8` sample). The
//! sum `predictor + mean` is computed in `i16` arithmetic to allow
//! for the inter path's negative mean and the intra path's
//! `255 + 255` overflow case (intra mean = `255`, predictor passed
//! as `255` in the INTRA-mode P/B-frame edge case — though §5.4.1
//! pins the intra path's predictor at `0`). The `saturate_u8`
//! clamp is then applied to the `i16` result to land back in
//! `[0, 255]`.
//!
//! ## No bitstream reads
//!
//! This module is pure arithmetic — it does not touch
//! [`crate::BitReader`]. The future round that wires the mean-VLC
//! tables will read the VLC, decode the mean value, and call into
//! these helpers for the per-sample apply step.

use crate::svq1_blocktree::Svq1Level;

/// Lower bound of the SVQ1 inter-half mean value range,
/// `[-256, +255]` per `docs/video/svq1/spec/05-mean-removal.md`
/// §5.1 / §5.1.2.
pub const INTER_MEAN_MIN: i16 = -256;

/// Upper bound of the SVQ1 inter-half mean value range,
/// `[-256, +255]` per `docs/video/svq1/spec/05-mean-removal.md`
/// §5.1 / §5.1.2.
pub const INTER_MEAN_MAX: i16 = 255;

/// Lower bound of the SVQ1 intra-half mean value range,
/// `[0, 255]` per `docs/video/svq1/spec/05-mean-removal.md` §5.1 /
/// §5.1.1. Exposed as a `u8` constant for type-safe use by
/// downstream callers.
pub const INTRA_MEAN_MIN: u8 = 0;

/// Upper bound of the SVQ1 intra-half mean value range,
/// `[0, 255]` per `docs/video/svq1/spec/05-mean-removal.md` §5.1 /
/// §5.1.1.
pub const INTRA_MEAN_MAX: u8 = 255;

/// Errors raised by the mean-step apply helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeanError {
    /// The inter mean value passed to [`apply_inter_mean_step`] is
    /// outside the closed range `[-256, +255]` documented in
    /// `docs/video/svq1/spec/05-mean-removal.md` §5.1 / §5.1.2.
    OutOfRange(i16),
}

impl core::fmt::Display for MeanError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MeanError::OutOfRange(value) => {
                write!(
                    f,
                    "oxideav-svq: inter mean value {value} outside [-256, +255] per spec/05 §5.1.2"
                )
            }
        }
    }
}

impl std::error::Error for MeanError {}

/// Clamp an arbitrary `i16` to the `[0, 255]` unsigned byte range
/// per `docs/video/svq1/spec/05-mean-removal.md` §5 / §5.4.3 — the
/// "saturate to an unsigned byte range, 0..255" step the wiki spec
/// mandates at each stage of the addition. Exposed as `pub const
/// fn` so the per-sample arithmetic of the mean step can be a
/// compile-time expression for fixed inputs.
pub const fn saturate_u8(value: i16) -> u8 {
    if value < 0 {
        0
    } else if value > 255 {
        255
    } else {
        value as u8
    }
}

/// Apply the intra-half mean step at a single sample position per
/// `docs/video/svq1/spec/05-mean-removal.md` §5.4.
///
/// The intra mean is an unsigned byte in `[0, 255]` per §5.1.1.
/// The intra path's predictor is unconditionally `0` per §5.4.1 —
/// but the helper accepts any `u8` predictor so the same
/// arithmetic also serves the INTRA-mode P/B-frame leaves
/// documented in §5.6's MB-coding table (those leaves use the
/// intra mean table per §5.6 row "P/B-frame INTRA → intra mean
/// table"; the predictor for those leaves is still zero per the
/// "Predictor" column of the same table).
///
/// The `saturate_u8` clamp is a no-op for the canonical intra
/// case (`predictor = 0`, `mean ∈ [0, 255]`) — the sum stays in
/// `[0, 255]`. The clamp is load-bearing only if a caller passes a
/// non-zero predictor (e.g. a future INTRA-mode call site that
/// reuses this helper for a non-canonical predictor).
pub const fn apply_intra_mean_step(predictor: u8, mean: u8) -> u8 {
    saturate_u8(predictor as i16 + mean as i16)
}

/// Apply the inter-half mean step at a single sample position per
/// `docs/video/svq1/spec/05-mean-removal.md` §5.4.
///
/// The inter mean is a signed value in `[-256, +255]` per §5.1.2 /
/// §5.1.3. The predictor is a `u8` motion-compensated reference
/// sample per §5.4. The sum is clamped to `[0, 255]` per the §5.4.3
/// per-stage saturation — the clamp is load-bearing on the inter
/// path (e.g. `predictor = 0`, `mean = -1` → clamped to `0`;
/// `predictor = 255`, `mean = +1` → clamped to `255`).
///
/// Returns [`MeanError::OutOfRange`] if the supplied mean value
/// falls outside `[-256, +255]`.
pub const fn apply_inter_mean_step(predictor: u8, mean: i16) -> Result<u8, MeanError> {
    if mean < INTER_MEAN_MIN || mean > INTER_MEAN_MAX {
        return Err(MeanError::OutOfRange(mean));
    }
    Ok(saturate_u8(predictor as i16 + mean))
}

/// The number of sample positions in a SVQ1 leaf block at level
/// `L`, per `docs/video/svq1/spec/05-mean-removal.md` §5.4 (the
/// `V_L` count) and `docs/video/svq1/spec/03-block-hierarchy.md`
/// §3.3 the level → block-shape mapping (L=0 4×2 = 8; L=1 4×4 =
/// 16; L=2 8×4 = 32; L=3 8×8 = 64). The mean is replicated across
/// ALL `V_L` positions per §5.4 — the per-sample arithmetic of
/// `apply_intra_mean_step` / `apply_inter_mean_step` is invoked
/// once per position.
///
/// Returns `None` for `L=4` / `L=5` since those levels do not host
/// a mean-removed VQ leaf (per `docs/video/svq1/spec/14.10-codebook-L4.md`
/// / `docs/video/svq1/spec/14.11-codebook-L5.md`).
pub const fn samples_per_leaf(level: Svq1Level) -> Option<usize> {
    match level {
        Svq1Level::L0 | Svq1Level::L1 | Svq1Level::L2 | Svq1Level::L3 => {
            Some(level.vector_length() as usize)
        }
        Svq1Level::L4 | Svq1Level::L5 => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturate_u8_clamps_below_zero_to_zero() {
        // `predictor = 0`, `mean = -256` (the inter minimum) →
        // `saturate_u8(0 + -256)` = `0` per §5.4.3 / §5.1.2.
        assert_eq!(saturate_u8(-256), 0);
        // `predictor = 0`, `mean = -1` → clamps to `0` per §5.4.3.
        assert_eq!(saturate_u8(-1), 0);
        // Boundary: `0` stays `0`.
        assert_eq!(saturate_u8(0), 0);
    }

    #[test]
    fn saturate_u8_clamps_above_255_to_255() {
        // `predictor = 255`, `mean = 255` (the intra max + intra
        // max edge) → `saturate_u8(255 + 255) = saturate_u8(510) =
        // 255` per §5.4.3.
        assert_eq!(saturate_u8(510), 255);
        // `predictor = 255`, `mean = 1` → `saturate_u8(256) = 255`.
        assert_eq!(saturate_u8(256), 255);
        // Boundary: `255` stays `255`.
        assert_eq!(saturate_u8(255), 255);
    }

    #[test]
    fn saturate_u8_passes_in_range_values_unchanged() {
        // Sweep the middle of the range — every value in
        // `[0, 255]` passes through unchanged per §5 "saturate to
        // an unsigned byte range, 0..255".
        for v in 0..=255i16 {
            assert_eq!(saturate_u8(v), v as u8);
        }
    }

    #[test]
    fn intra_mean_only_block_with_zero_predictor_equals_mean() {
        // Per spec/05 §5.4.1 — for a mean-only intra leaf
        // (`predictor = 0`) the entire block is filled with the
        // mean (the clamp is a no-op since intra mean is `u8`).
        // The wiki worked example cited in §5.9 uses `mean = 61`;
        // we cover that worked example here plus the boundary
        // values `0` and `255`.
        assert_eq!(apply_intra_mean_step(0, 61), 61);
        assert_eq!(apply_intra_mean_step(0, INTRA_MEAN_MIN), 0);
        assert_eq!(apply_intra_mean_step(0, INTRA_MEAN_MAX), 255);
    }

    #[test]
    fn intra_mean_with_non_zero_predictor_saturates() {
        // Non-canonical case (INTRA-mode P/B-frame call site per
        // §5.6 — predictor is still zero in the canonical row but
        // an implementation that reuses this helper with a
        // non-zero predictor must still see the clamp behave). At
        // `predictor = 200`, `mean = 200`, the sum is `400`,
        // clamped to `255` per §5.4.3.
        assert_eq!(apply_intra_mean_step(200, 200), 255);
        // Mid-range sum that does not saturate stays exact.
        assert_eq!(apply_intra_mean_step(100, 60), 160);
    }

    #[test]
    fn inter_mean_negative_residue_against_low_predictor_clamps_to_zero() {
        // Per spec/05 §5.1.2 — "an inter mean of `-256` added to
        // a predictor of `0` saturates to `0`."
        assert_eq!(apply_inter_mean_step(0, -256), Ok(0));
        // `predictor = 50`, `mean = -100` → `-50` → clamps to 0.
        assert_eq!(apply_inter_mean_step(50, -100), Ok(0));
        // Boundary: `predictor = 100`, `mean = -100` → 0 exactly.
        assert_eq!(apply_inter_mean_step(100, -100), Ok(0));
    }

    #[test]
    fn inter_mean_positive_residue_against_high_predictor_clamps_to_255() {
        // Per spec/05 §5.1.2 — "an inter mean of `+255` added to
        // a predictor of `255` saturates to `255`."
        assert_eq!(apply_inter_mean_step(255, 255), Ok(255));
        // `predictor = 200`, `mean = 100` → 300 → clamps to 255.
        assert_eq!(apply_inter_mean_step(200, 100), Ok(255));
        // Boundary: `predictor = 128`, `mean = 127` → 255 exactly.
        assert_eq!(apply_inter_mean_step(128, 127), Ok(255));
    }

    #[test]
    fn inter_mean_zero_residue_returns_predictor_unchanged() {
        // Per spec/05 §5.6.1 — inter means cluster strongly around
        // `0` (perfect motion prediction → zero residue). The
        // mean step at `mean = 0` returns the predictor unchanged
        // for every predictor in `[0, 255]`.
        for predictor in 0..=255u8 {
            assert_eq!(apply_inter_mean_step(predictor, 0), Ok(predictor));
        }
    }

    #[test]
    fn inter_mean_out_of_range_rejects() {
        // Per spec/05 §5.1.2 — the inter mean's domain is closed
        // `[-256, +255]`. Anything outside is an out-of-range
        // input the helper rejects.
        assert_eq!(
            apply_inter_mean_step(0, INTER_MEAN_MIN - 1),
            Err(MeanError::OutOfRange(-257))
        );
        assert_eq!(
            apply_inter_mean_step(0, INTER_MEAN_MAX + 1),
            Err(MeanError::OutOfRange(256))
        );
    }

    #[test]
    fn samples_per_leaf_matches_block_shape() {
        // Per spec/03 §3.3 — `V_L` = 8 / 16 / 32 / 64 for
        // L=0..L=3. The mean is replicated across all `V_L`
        // positions per spec/05 §5.4.
        assert_eq!(samples_per_leaf(Svq1Level::L0), Some(8));
        assert_eq!(samples_per_leaf(Svq1Level::L1), Some(16));
        assert_eq!(samples_per_leaf(Svq1Level::L2), Some(32));
        assert_eq!(samples_per_leaf(Svq1Level::L3), Some(64));
        // Per spec/14.10 / §14.11 — L=4 / L=5 do not host a
        // mean-removed VQ leaf.
        assert_eq!(samples_per_leaf(Svq1Level::L4), None);
        assert_eq!(samples_per_leaf(Svq1Level::L5), None);
    }

    #[test]
    fn intra_mean_range_constants_match_spec() {
        // Per spec/05 §5.1.1 — intra mean range is `[0, 255]`.
        assert_eq!(INTRA_MEAN_MIN, 0);
        assert_eq!(INTRA_MEAN_MAX, 255);
    }

    #[test]
    fn inter_mean_range_constants_match_spec() {
        // Per spec/05 §5.1.2 — inter mean range is `[-256, +255]`.
        assert_eq!(INTER_MEAN_MIN, -256);
        assert_eq!(INTER_MEAN_MAX, 255);
    }
}
