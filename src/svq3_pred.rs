//! SVQ3 intra-prediction pixel arithmetic (structural).
//!
//! ## Provenance
//!
//! Round 282 implements the one intra predictor the wiki spec pins
//! completely in `docs/video/svq3/wiki/Sorenson_Video_3.wiki`
//! §"Intra prediction" (verbatim local mirror of the multimedia.cx
//! `Sorenson_Video_3` wiki page). The section opens with "Intra
//! prediction is the same as in H.264 except for the following
//! quirks" and then spells the first quirk out in full:
//!
//! > 4x4 diagonal down prediction is performed as
//! >
//! > ```text
//! >   a b c c
//! >   b c c c
//! >   c c c c
//! >   c c c c
//! > ```
//! >
//! > where `a = (left[1] + top[1]) / 2`, `b = (left[2] + top[2]) / 2`
//! > and `c = (left[3] + top[3]) / 2`.
//!
//! Both halves of that quirk land here verbatim: the three per-sample
//! closed forms as the [`diagonal_down_sample`] `const fn` (one
//! neighbour-pair average with the spec's plain integer `/ 2` — no
//! rounding bias is present in the spec text, and the operands are
//! non-negative samples so the division is a floor), and the 4×4
//! fill picture as the [`DIAGONAL_DOWN_PATTERN`] placement table plus
//! the [`predict_diagonal_down_4x4`] block predictor that combines
//! the two.
//!
//! The spec formulas consume `left[1..=3]` and `top[1..=3]` only;
//! element `0` of either neighbour array is never referenced by this
//! predictor. The three consumed indices are surfaced as
//! [`DIAGONAL_DOWN_NEIGHBOUR_INDICES`] so callers assembling the
//! neighbour arrays can corroborate the layout.
//!
//! ## Open work
//!
//! * The numeric intra-prediction-mode value (`0..=4` in
//!   [`crate::svq3_mb::INTRA_PRED_TABLE`]) that selects this
//!   predictor is NOT pinned in `docs/video/svq3/` — the wiki names
//!   the predictor ("diagonal down") without binding it to a mode
//!   number. The dispatch table that routes a resolved mode to a
//!   predictor function is deferred until docs pin the binding.
//! * The remaining 4×4 intra predictors, the 16×16 predictors (the
//!   wiki pins only "plane prediction is the same as in H.264 but
//!   transposed"), and the chroma DC predictor ("8x8 chroma always
//!   uses DC prediction") are back-referenced to H.264 rather than
//!   spelled out locally — `docs/video/svq3/spec/01-reconstruction-composition.md`
//!   Gap 4 now carries their sample equations; their block predictors
//!   are deferred to a later round.
//!
//! ## Reconstruction-composition writeback (spec/01 Gap 5)
//!
//! `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 5 pins
//! the predicted+residual writeback as the standard 8-bit saturating
//! sum with **no extra rounding on the add** — all rounding already
//! lives inside the dequant/transform pass
//! ([`crate::svq3_dequant`]) and the interpolation filters:
//!
//! > ```text
//! > recon[x,y] = Clip1( pred[x,y] + residual[x,y] )
//! >            = clip( pred[x,y] + residual[x,y], 0, 255 )
//! > ```
//!
//! That writeback lands here as [`reconstruct_sample`] (one clamped
//! sum) and [`reconstruct_4x4`] (the 4×4-block composition that takes
//! a predicted `[u8; 16]` block — e.g. from
//! [`predict_diagonal_down_4x4`] — and the dequantised/transformed
//! residual `[i32; 16]` from [`crate::svq3_dequant`] and produces the
//! reconstructed `[u8; 16]`). The clamp is the ordinary H.264
//! `Clip1_Y` / `Clip1_C` at `BitDepth = 8` ⇒ range `[0, 255]`
//! ([`RECON_SAMPLE_MIN`] / [`RECON_SAMPLE_MAX`]).
//!
//! ## Open work
//!
//! `Svq3DecoderHandle::receive_frame` continues to return
//! `oxideav_core::Error::Unsupported` — this module lands the
//! per-block pixel arithmetic and writeback composition only; the
//! macroblock loop that drives predictor selection, residual decode,
//! and writeback is not yet assembled.

/// Width / height of the 4×4 intra-predicted sub-block, and the
/// length of the `left` / `top` neighbour arrays the spec formulas
/// index into.
pub const PRED_4X4_DIM: usize = 4;

/// Number of samples in one 4×4 predicted block
/// (`PRED_4X4_DIM * PRED_4X4_DIM`).
pub const PRED_4X4_SAMPLES: usize = PRED_4X4_DIM * PRED_4X4_DIM;

/// The neighbour-array indices the spec's three closed forms consume,
/// in `(a, b, c)` order: `a` averages `left[1]` / `top[1]`, `b`
/// averages `left[2]` / `top[2]`, `c` averages `left[3]` / `top[3]`.
///
/// Element `0` of either neighbour array is never referenced by the
/// diagonal-down predictor.
pub const DIAGONAL_DOWN_NEIGHBOUR_INDICES: [usize; 3] = [1, 2, 3];

/// The 4×4 fill picture from the wiki spec's §"Intra prediction",
/// flattened row-major (`DIAGONAL_DOWN_PATTERN[row * 4 + col]`):
///
/// ```text
///   a b c c
///   b c c c
///   c c c c
///   c c c c
/// ```
///
/// Each entry selects one of the three derived samples: `0` ⇒ `a`,
/// `1` ⇒ `b`, `2` ⇒ `c`.
#[rustfmt::skip]
pub const DIAGONAL_DOWN_PATTERN: [u8; PRED_4X4_SAMPLES] = [
    0, 1, 2, 2,
    1, 2, 2, 2,
    2, 2, 2, 2,
    2, 2, 2, 2,
];

/// One diagonal-down predicted sample: the wiki spec's
/// `(left[k] + top[k]) / 2` closed form applied to a single
/// neighbour pair.
///
/// The spec writes a plain integer `/ 2` with no rounding bias; both
/// operands are unsigned samples so the division is an exact floor.
/// The result always fits in `u8` (`(255 + 255) / 2 = 255`).
///
/// ```
/// use oxideav_svq::svq3_pred::diagonal_down_sample;
///
/// // c = (left[3] + top[3]) / 2 with left[3] = 30, top[3] = 31:
/// // floor(61 / 2) = 30.
/// assert_eq!(diagonal_down_sample(30, 31), 30);
/// ```
pub const fn diagonal_down_sample(left_k: u8, top_k: u8) -> u8 {
    ((left_k as u16 + top_k as u16) / 2) as u8
}

/// The 4×4 diagonal-down intra predictor from the wiki spec's
/// §"Intra prediction", combining the three
/// [`diagonal_down_sample`] closed forms (`a` / `b` / `c` from
/// neighbour indices `1` / `2` / `3`) with the
/// [`DIAGONAL_DOWN_PATTERN`] fill picture.
///
/// `left` and `top` are the previously-reconstructed neighbour
/// sample arrays the spec formulas index into; element `0` of either
/// array is never referenced. The return value is the predicted 4×4
/// block flattened row-major (`out[row * 4 + col]`), matching the
/// row-major block layout used by [`crate::svq3_scan`] and
/// [`crate::svq3_dequant`], so the eventual predicted+residual
/// writeback can combine the two element-wise.
///
/// ```
/// use oxideav_svq::svq3_pred::predict_diagonal_down_4x4;
///
/// let left = [9, 10, 20, 30];
/// let top = [7, 14, 21, 31];
/// // a = (10 + 14) / 2 = 12, b = (20 + 21) / 2 = 20,
/// // c = (30 + 31) / 2 = 30.
/// assert_eq!(
///     predict_diagonal_down_4x4(left, top),
///     [
///         12, 20, 30, 30, //
///         20, 30, 30, 30, //
///         30, 30, 30, 30, //
///         30, 30, 30, 30, //
///     ]
/// );
/// ```
pub const fn predict_diagonal_down_4x4(
    left: [u8; PRED_4X4_DIM],
    top: [u8; PRED_4X4_DIM],
) -> [u8; PRED_4X4_SAMPLES] {
    let derived = [
        diagonal_down_sample(left[1], top[1]),
        diagonal_down_sample(left[2], top[2]),
        diagonal_down_sample(left[3], top[3]),
    ];
    let mut out = [0u8; PRED_4X4_SAMPLES];
    let mut i = 0;
    while i < PRED_4X4_SAMPLES {
        out[i] = derived[DIAGONAL_DOWN_PATTERN[i] as usize];
        i += 1;
    }
    out
}

/// Minimum reconstructed sample value — the lower bound of the
/// spec/01 Gap 5 `Clip1` saturating clamp at `BitDepth = 8`.
///
/// Per `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 5
/// the writeback clamp is the ordinary H.264 `Clip1_Y` / `Clip1_C`
/// with `BitDepth = 8`, i.e. `clip(·, 0, 255)`.
pub const RECON_SAMPLE_MIN: i32 = 0;

/// Maximum reconstructed sample value — the upper bound of the
/// spec/01 Gap 5 `Clip1` saturating clamp at `BitDepth = 8`
/// (`(1 << 8) - 1 = 255`).
pub const RECON_SAMPLE_MAX: i32 = 255;

/// Compose one reconstructed sample from a predicted sample and its
/// residual, applying the spec/01 Gap 5 saturating clamp.
///
/// `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 5 pins
/// per-sample reconstruction as
///
/// ```text
///   recon[x,y] = Clip1( pred[x,y] + residual[x,y] )
///              = clip( pred[x,y] + residual[x,y], 0, 255 )
/// ```
///
/// with **no per-sample rounding term on the add itself** — all
/// rounding already lives inside the dequant/transform pass
/// ([`crate::svq3_dequant`], the fused `+0x80000 … >> 20`) and the
/// interpolation filters. This helper therefore performs the plain
/// signed sum `pred + residual` and clamps it into
/// `[RECON_SAMPLE_MIN, RECON_SAMPLE_MAX]` (`[0, 255]`).
///
/// `pred` is a previously-produced predicted sample (intra predictor
/// output such as [`predict_diagonal_down_4x4`], or an inter
/// motion-compensated predictor); `residual` is the inverse-
/// transformed, dequantised coefficient from [`crate::svq3_dequant`].
/// The sum is computed in `i64` before clamping so a residual that
/// drives the sum below `0` or above `255` — including a pathological
/// residual at the `i32` extremes — saturates rather than wrapping.
///
/// ```
/// use oxideav_svq::svq3_pred::reconstruct_sample;
///
/// // In-range sum passes through unchanged.
/// assert_eq!(reconstruct_sample(100, 27), 127);
/// // Negative residual that underflows saturates to 0.
/// assert_eq!(reconstruct_sample(10, -50), 0);
/// // Positive residual that overflows saturates to 255.
/// assert_eq!(reconstruct_sample(200, 100), 255);
/// ```
#[inline]
#[must_use]
pub const fn reconstruct_sample(pred: u8, residual: i32) -> u8 {
    // Widen to i64 so the add cannot overflow even for a pathological
    // residual at the i32 extremes; the clamp then bounds it to [0, 255].
    let sum = pred as i64 + residual as i64;
    let clamped = if sum < RECON_SAMPLE_MIN as i64 {
        RECON_SAMPLE_MIN
    } else if sum > RECON_SAMPLE_MAX as i64 {
        RECON_SAMPLE_MAX
    } else {
        sum as i32
    };
    clamped as u8
}

/// Compose a reconstructed 4×4 block from a predicted block and its
/// residual block, applying the spec/01 Gap 5 saturating clamp
/// element-wise.
///
/// This is the per-block form of [`reconstruct_sample`]: it walks the
/// two row-major `[_; 16]` blocks in lockstep and writes
/// `Clip1(pred[i] + residual[i])` at each position. Both inputs use
/// the same row-major 4×4 layout (`block[row * 4 + col]`) as
/// [`predict_diagonal_down_4x4`] and the
/// [`crate::svq3_dequant`] transform output, so the reconstructed
/// block is laid out the same way.
///
/// `predicted` is the intra/inter predictor output for the block;
/// `residual` is the dequantised, inverse-transformed coefficient
/// block from [`crate::svq3_dequant`] (already rounded by its fused
/// `+0x80000 … >> 20`). No additional rounding is applied to the sum,
/// per spec/01 Gap 5.
///
/// ```
/// use oxideav_svq::svq3_pred::{predict_diagonal_down_4x4, reconstruct_4x4};
///
/// // A uniform predictor plus an all-zero residual reproduces the
/// // prediction; a non-zero residual at a position shifts that sample.
/// let pred = predict_diagonal_down_4x4([5; 4], [5; 4]); // all 5s
/// let mut residual = [0i32; 16];
/// residual[0] = 10;
/// residual[15] = -100; // underflows -> clamps to 0
/// let recon = reconstruct_4x4(pred, residual);
/// assert_eq!(recon[0], 15);
/// assert_eq!(recon[1], 5);
/// assert_eq!(recon[15], 0);
/// ```
#[inline]
#[must_use]
pub const fn reconstruct_4x4(
    predicted: [u8; PRED_4X4_SAMPLES],
    residual: [i32; PRED_4X4_SAMPLES],
) -> [u8; PRED_4X4_SAMPLES] {
    let mut out = [0u8; PRED_4X4_SAMPLES];
    let mut i = 0;
    while i < PRED_4X4_SAMPLES {
        out[i] = reconstruct_sample(predicted[i], residual[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_matches_spec_picture() {
        // Row-major transcription of the wiki picture:
        //   a b c c / b c c c / c c c c / c c c c
        #[rustfmt::skip]
        let expected: [u8; 16] = [
            0, 1, 2, 2,
            1, 2, 2, 2,
            2, 2, 2, 2,
            2, 2, 2, 2,
        ];
        assert_eq!(DIAGONAL_DOWN_PATTERN, expected);
    }

    #[test]
    fn pattern_kind_population() {
        // The picture places `a` once, `b` twice, `c` thirteen times.
        let mut counts = [0usize; 3];
        for &kind in DIAGONAL_DOWN_PATTERN.iter() {
            assert!(kind <= 2, "pattern entry out of range: {kind}");
            counts[kind as usize] += 1;
        }
        assert_eq!(counts, [1, 2, 13]);
    }

    #[test]
    fn neighbour_indices_are_one_two_three() {
        assert_eq!(DIAGONAL_DOWN_NEIGHBOUR_INDICES, [1, 2, 3]);
        for &idx in DIAGONAL_DOWN_NEIGHBOUR_INDICES.iter() {
            assert!(idx < PRED_4X4_DIM);
        }
    }

    #[test]
    fn dimension_constants_agree() {
        assert_eq!(PRED_4X4_DIM, 4);
        assert_eq!(PRED_4X4_SAMPLES, 16);
        assert_eq!(DIAGONAL_DOWN_PATTERN.len(), PRED_4X4_SAMPLES);
    }

    #[test]
    fn sample_zero_and_max_bounds() {
        assert_eq!(diagonal_down_sample(0, 0), 0);
        assert_eq!(diagonal_down_sample(255, 255), 255);
        assert_eq!(diagonal_down_sample(255, 0), 127);
        assert_eq!(diagonal_down_sample(0, 255), 127);
    }

    #[test]
    fn sample_floor_division() {
        // The spec writes a plain `/ 2`: odd sums floor.
        assert_eq!(diagonal_down_sample(1, 2), 1);
        assert_eq!(diagonal_down_sample(2, 1), 1);
        assert_eq!(diagonal_down_sample(0, 1), 0);
        assert_eq!(diagonal_down_sample(3, 0), 1);
        assert_eq!(diagonal_down_sample(10, 20), 15);
        assert_eq!(diagonal_down_sample(61, 0), 30);
    }

    #[test]
    fn sample_is_symmetric_in_left_and_top() {
        let sweep = [0u8, 1, 2, 61, 127, 128, 200, 254, 255];
        for &l in sweep.iter() {
            for &t in sweep.iter() {
                assert_eq!(
                    diagonal_down_sample(l, t),
                    diagonal_down_sample(t, l),
                    "asymmetric at ({l}, {t})"
                );
            }
        }
    }

    #[test]
    fn predict_uniform_neighbours_reproduce_the_value() {
        for &v in [0u8, 1, 61, 127, 200, 255].iter() {
            let out = predict_diagonal_down_4x4([v; 4], [v; 4]);
            assert_eq!(out, [v; 16], "uniform value {v} not reproduced");
        }
    }

    #[test]
    fn predict_matches_the_three_closed_forms() {
        let left = [200u8, 17, 48, 99];
        let top = [3u8, 250, 5, 130];
        let a = diagonal_down_sample(left[1], top[1]);
        let b = diagonal_down_sample(left[2], top[2]);
        let c = diagonal_down_sample(left[3], top[3]);
        let out = predict_diagonal_down_4x4(left, top);
        assert_eq!(out[0], a);
        assert_eq!(out[1], b);
        assert_eq!(out[4], b);
        for (i, &sample) in out.iter().enumerate() {
            if i != 0 && i != 1 && i != 4 {
                assert_eq!(sample, c, "position {i} should carry c");
            }
        }
    }

    #[test]
    fn predict_row_layout_matches_picture() {
        let left = [0u8, 10, 30, 50];
        let top = [0u8, 14, 32, 52];
        let a = 12; // (10 + 14) / 2
        let b = 31; // (30 + 32) / 2
        let c = 51; // (50 + 52) / 2
        let out = predict_diagonal_down_4x4(left, top);
        let rows: [[u8; 4]; 4] = [[a, b, c, c], [b, c, c, c], [c, c, c, c], [c, c, c, c]];
        for (r, row) in rows.iter().enumerate() {
            for (col, &expected) in row.iter().enumerate() {
                assert_eq!(out[r * 4 + col], expected, "mismatch at ({r}, {col})");
            }
        }
    }

    #[test]
    fn predict_ignores_neighbour_index_zero() {
        let left = [0u8, 11, 22, 33];
        let top = [0u8, 44, 55, 66];
        let baseline = predict_diagonal_down_4x4(left, top);
        for &noise in [1u8, 128, 255].iter() {
            let mut l = left;
            let mut t = top;
            l[0] = noise;
            t[0] = noise.wrapping_add(7);
            assert_eq!(
                predict_diagonal_down_4x4(l, t),
                baseline,
                "element 0 leaked into the prediction (noise {noise})"
            );
        }
    }

    #[test]
    fn predict_is_symmetric_in_left_and_top() {
        let left = [5u8, 90, 180, 240];
        let top = [200u8, 15, 60, 1];
        assert_eq!(
            predict_diagonal_down_4x4(left, top),
            predict_diagonal_down_4x4(top, left)
        );
    }

    #[test]
    fn predict_agrees_with_pattern_indexing() {
        let left = [77u8, 1, 254, 128];
        let top = [12u8, 255, 0, 129];
        let derived = [
            diagonal_down_sample(left[1], top[1]),
            diagonal_down_sample(left[2], top[2]),
            diagonal_down_sample(left[3], top[3]),
        ];
        let out = predict_diagonal_down_4x4(left, top);
        for (i, &sample) in out.iter().enumerate() {
            assert_eq!(sample, derived[DIAGONAL_DOWN_PATTERN[i] as usize]);
        }
    }

    #[test]
    fn predict_worked_example() {
        let out = predict_diagonal_down_4x4([9, 10, 20, 30], [7, 14, 21, 31]);
        #[rustfmt::skip]
        let expected: [u8; 16] = [
            12, 20, 30, 30,
            20, 30, 30, 30,
            30, 30, 30, 30,
            30, 30, 30, 30,
        ];
        assert_eq!(out, expected);
    }

    #[test]
    fn helpers_are_const_usable() {
        const SAMPLE: u8 = diagonal_down_sample(30, 31);
        const BLOCK: [u8; PRED_4X4_SAMPLES] = predict_diagonal_down_4x4([0, 2, 4, 6], [0, 2, 4, 6]);
        assert_eq!(SAMPLE, 30);
        assert_eq!(BLOCK[0], 2);
        assert_eq!(BLOCK[1], 4);
        assert_eq!(BLOCK[15], 6);
    }

    // ---- spec/01 Gap 5: predicted+residual writeback composition -----

    #[test]
    fn recon_clamp_bounds_match_8bit() {
        assert_eq!(RECON_SAMPLE_MIN, 0);
        assert_eq!(RECON_SAMPLE_MAX, 255);
        assert_eq!(RECON_SAMPLE_MAX, (1i32 << 8) - 1);
    }

    #[test]
    fn recon_sample_in_range_is_plain_sum() {
        // No rounding term on the add — the in-range case is the exact
        // signed sum pred + residual.
        assert_eq!(reconstruct_sample(0, 0), 0);
        assert_eq!(reconstruct_sample(100, 27), 127);
        assert_eq!(reconstruct_sample(255, 0), 255);
        assert_eq!(reconstruct_sample(128, -28), 100);
        assert_eq!(reconstruct_sample(0, 255), 255);
    }

    #[test]
    fn recon_sample_saturates_low() {
        assert_eq!(reconstruct_sample(10, -50), 0);
        assert_eq!(reconstruct_sample(0, -1), 0);
        assert_eq!(reconstruct_sample(0, i32::MIN), 0);
        assert_eq!(reconstruct_sample(127, -128), 0); // exactly 0, not below
        assert_eq!(reconstruct_sample(127, -127), 0);
    }

    #[test]
    fn recon_sample_saturates_high() {
        assert_eq!(reconstruct_sample(200, 100), 255);
        assert_eq!(reconstruct_sample(255, 1), 255);
        assert_eq!(reconstruct_sample(255, i32::MAX), 255);
        assert_eq!(reconstruct_sample(200, 55), 255); // exactly 255
        assert_eq!(reconstruct_sample(200, 56), 255); // one over -> clamps
    }

    #[test]
    fn recon_sample_zero_residual_is_identity() {
        for pred in 0u8..=255 {
            assert_eq!(reconstruct_sample(pred, 0), pred, "pred {pred}");
        }
    }

    #[test]
    fn recon_4x4_zero_residual_reproduces_prediction() {
        let pred = predict_diagonal_down_4x4([9, 10, 20, 30], [7, 14, 21, 31]);
        let recon = reconstruct_4x4(pred, [0i32; PRED_4X4_SAMPLES]);
        assert_eq!(recon, pred);
    }

    #[test]
    fn recon_4x4_is_elementwise_clamped_sum() {
        let pred = predict_diagonal_down_4x4([5; 4], [5; 4]); // all 5s
        let mut residual = [0i32; PRED_4X4_SAMPLES];
        residual[0] = 10;
        residual[7] = 250; // 5 + 250 = 255
        residual[8] = 251; // 5 + 251 = 256 -> clamps to 255
        residual[15] = -100; // 5 - 100 -> clamps to 0
        let recon = reconstruct_4x4(pred, residual);
        assert_eq!(recon[0], 15);
        assert_eq!(recon[1], 5);
        assert_eq!(recon[7], 255);
        assert_eq!(recon[8], 255);
        assert_eq!(recon[15], 0);
        // Every other position is the untouched prediction (5).
        for i in [2usize, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14] {
            assert_eq!(recon[i], 5, "position {i}");
        }
    }

    #[test]
    fn recon_4x4_matches_per_sample_helper() {
        let pred = predict_diagonal_down_4x4([200, 17, 48, 99], [3, 250, 5, 130]);
        let residual: [i32; PRED_4X4_SAMPLES] = [
            0, 5, -300, 400, 1, -1, 127, -127, 255, -255, 12, -12, 50, -50, 200, -200,
        ];
        let block = reconstruct_4x4(pred, residual);
        for i in 0..PRED_4X4_SAMPLES {
            assert_eq!(
                block[i],
                reconstruct_sample(pred[i], residual[i]),
                "position {i}"
            );
        }
    }

    #[test]
    fn recon_helpers_are_const_usable() {
        const SAMPLE: u8 = reconstruct_sample(100, 27);
        const BLOCK: [u8; PRED_4X4_SAMPLES] = reconstruct_4x4([5u8; 16], [10i32; 16]);
        assert_eq!(SAMPLE, 127);
        assert_eq!(BLOCK, [15u8; 16]);
    }
}
