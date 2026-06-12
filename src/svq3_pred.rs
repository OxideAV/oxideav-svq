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
//!   spelled out locally — they stay out of scope until
//!   `docs/video/svq3/` carries their sample equations.
//! * The predicted+residual writeback (clamp range / rounding of
//!   `predicted + residual`) is not pinned in `docs/video/svq3/`
//!   either; this module produces the predicted block only.
//!
//! `Svq3DecoderHandle::receive_frame` continues to return
//! `oxideav_core::Error::Unsupported` — round 282 lands the pixel
//! arithmetic only.

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
}
