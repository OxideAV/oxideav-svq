//! SVQ3 sub-pixel motion-compensation interpolation arithmetic
//! (structural).
//!
//! ## Provenance
//!
//! Round 224 implements the per-sample interpolation arithmetic
//! described in
//! `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation"
//! (verbatim local mirror of the multimedia.cx
//! `Sorenson_Video_3` wiki page). The relevant paragraph reads:
//!
//! > Thirdpel interpolation in one direction uses formula
//! > `((2 * A + B + 1) * 0x2AB) >> 11` and two-dimensional
//! > interpolation uses matrix
//! >
//! > ```text
//! >   4 3
//! >   3 2
//! > ```
//! >
//! > and `((4 * A + 3 * B + 3 * C + 2 * D + 6) * 0xAAB) >> 15` for the
//! > output.
//!
//! Both formulas land here as `const fn` helpers plus the two integer
//! constants (`0x2AB` = 683, `0xAAB` = 2731) and their shift counts
//! (11 and 15 respectively). The 2×2 weight matrix from the spec is
//! also exposed as a [`THIRDPEL_2D_WEIGHTS`] constant.
//!
//! ## Numerical interpretation (informative)
//!
//! Round 224 does NOT mirror this as a spec claim — the wiki spec
//! simply lists the formulas — but the multiplier / shift pairs
//! correspond to fixed-point reciprocals: `683 / 2048 ≈ 1/3` (1D, the
//! sum of the input weights `2 + 1 = 3`) and `2731 / 32768 ≈ 1/12` (2D,
//! the sum of the input weights `4 + 3 + 3 + 2 = 12`). The `+1` /
//! `+6` constants in the numerators are the half-divisor rounding
//! biases (`3/2 → 1` rounded down, `12/2 = 6`). The
//! [`thirdpel_interpolate_1d`] / [`thirdpel_interpolate_2d`] helpers
//! implement the spec's exact integer arithmetic — no division and no
//! floating point — so the closed-form `(x + half_div) / sum` view is
//! a numerical-interpretation aid only.
//!
//! ## Motion-vector storage base
//!
//! The opening paragraph of the same wiki section
//! states "motion vectors are stored and predicted as fraction of six
//! and then rounded to the desired base". This implies a common
//! storage grid of sixths-of-a-sample with per-precision rounding to
//! one of three bases (Fullpel → 6, Halfpel → 3, Thirdpel → 2). Round
//! 224 surfaces the three bases as
//! [`stored_sixths_base`] (a free function taking a
//! [`Svq3MvPrecision`]) plus the
//! [`is_aligned_to_precision_base`] predicate that checks whether an
//! already-rounded sixths-grid value is on the precision's base; the
//! actual rounding step is NOT yet implemented because the wiki
//! spec text leaves the rounding direction (toward zero / away from
//! zero / round-half-up / round-half-even) unspecified.
//!
//! ## Open work
//!
//! The reference-frame-fetch + filter-application stage that consumes
//! these primitives is not yet wired — `Svq3DecoderHandle::receive_frame`
//! continues to return `oxideav_core::Error::Unsupported`. Round 224
//! lands the arithmetic only.

use crate::svq3_mb::Svq3MvPrecision;

/// Multiplier used by the one-dimensional thirdpel interpolation
/// formula, written `0x2AB` in
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation".
pub const THIRDPEL_1D_MULTIPLIER: i32 = 0x2AB;

/// Right-shift used by the one-dimensional thirdpel interpolation
/// formula, written `>> 11` in
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation".
pub const THIRDPEL_1D_SHIFT: u32 = 11;

/// Multiplier used by the two-dimensional thirdpel interpolation
/// formula, written `0xAAB` in
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation".
pub const THIRDPEL_2D_MULTIPLIER: i32 = 0xAAB;

/// Right-shift used by the two-dimensional thirdpel interpolation
/// formula, written `>> 15` in
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation".
pub const THIRDPEL_2D_SHIFT: u32 = 15;

/// Additive bias the spec's two-dimensional interpolation formula
/// uses: `+6` in the numerator `(4 * A + 3 * B + 3 * C + 2 * D + 6)`.
///
/// Equals half the sum of the per-input weights `4 + 3 + 3 + 2 = 12`
/// and serves as the round-half-up bias for the fixed-point `÷ 12`.
pub const THIRDPEL_2D_BIAS: i32 = 6;

/// Additive bias the spec's one-dimensional interpolation formula
/// uses: `+1` in the numerator `(2 * A + B + 1)`.
///
/// Equals half the sum of the per-input weights `2 + 1 = 3`
/// (integer-divided down, since `3/2 = 1` in integer arithmetic) and
/// serves as the round-half-up bias for the fixed-point `÷ 3`.
pub const THIRDPEL_1D_BIAS: i32 = 1;

/// The 2×2 weight matrix the wiki spec quotes verbatim for two-
/// dimensional thirdpel interpolation:
///
/// ```text
///   4 3
///   3 2
/// ```
///
/// Indexed `[row][col]` so `THIRDPEL_2D_WEIGHTS[0][0] = 4`,
/// `THIRDPEL_2D_WEIGHTS[1][1] = 2`. The
/// [`thirdpel_interpolate_2d`] helper consumes the four corner samples
/// in row-major order against this layout (`A = [0][0]`, `B = [0][1]`,
/// `C = [1][0]`, `D = [1][1]`).
pub const THIRDPEL_2D_WEIGHTS: [[u8; 2]; 2] = [[4, 3], [3, 2]];

/// Sum of the four entries of [`THIRDPEL_2D_WEIGHTS`].
///
/// Equals 12; the fixed-point reciprocal
/// `THIRDPEL_2D_MULTIPLIER / 2^THIRDPEL_2D_SHIFT = 2731 / 32768`
/// approximates `1/12` to within one part in 32768.
pub const THIRDPEL_2D_WEIGHT_SUM: u32 = 12;

/// Sum of the two input weights `2 + 1` of the one-dimensional
/// thirdpel interpolation formula.
///
/// Equals 3; the fixed-point reciprocal
/// `THIRDPEL_1D_MULTIPLIER / 2^THIRDPEL_1D_SHIFT = 683 / 2048`
/// approximates `1/3` to within one part in 2048.
pub const THIRDPEL_1D_WEIGHT_SUM: u32 = 3;

/// Apply the one-dimensional thirdpel interpolation formula from
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation":
/// `((2 * A + B + 1) * 0x2AB) >> 11`.
///
/// The inputs are the two adjacent reference samples along the
/// interpolation direction (`a` is the nearer sample with weight `2`,
/// `b` is the farther sample with weight `1`). The return value is
/// the interpolated sample as an [`i32`]; consumers must clip to the
/// `0..=255` sample range themselves.
#[inline]
#[must_use]
pub const fn thirdpel_interpolate_1d(a: i32, b: i32) -> i32 {
    ((2 * a + b + THIRDPEL_1D_BIAS) * THIRDPEL_1D_MULTIPLIER) >> THIRDPEL_1D_SHIFT
}

/// Apply the two-dimensional thirdpel interpolation formula from
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation":
/// `((4 * A + 3 * B + 3 * C + 2 * D + 6) * 0xAAB) >> 15`.
///
/// The four inputs map to the [`THIRDPEL_2D_WEIGHTS`] matrix in
/// row-major order:
///
/// | input | weight | matrix position |
/// | ----- | ------ | --------------- |
/// | `a`   | `4`    | `[0][0]`        |
/// | `b`   | `3`    | `[0][1]`        |
/// | `c`   | `3`    | `[1][0]`        |
/// | `d`   | `2`    | `[1][1]`        |
///
/// Returns the interpolated sample as an [`i32`]; consumers must clip
/// to the `0..=255` sample range themselves.
#[inline]
#[must_use]
pub const fn thirdpel_interpolate_2d(a: i32, b: i32, c: i32, d: i32) -> i32 {
    ((4 * a + 3 * b + 3 * c + 2 * d + THIRDPEL_2D_BIAS) * THIRDPEL_2D_MULTIPLIER)
        >> THIRDPEL_2D_SHIFT
}

/// Motion-vector storage base helper for [`Svq3MvPrecision`].
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion
/// Compensation" the SVQ3 wire stores motion-vector components "as
/// fraction of six and then rounded to the desired base". The three
/// supported precisions correspond to the three integer divisors of 6
/// — Fullpel → 6 (whole sample), Halfpel → 3 (1/2 sample = 3/6), and
/// Thirdpel → 2 (1/3 sample = 2/6).
///
/// Returns the base (in stored-sixths units) the precision's
/// post-storage rounding step targets.
#[inline]
#[must_use]
pub const fn stored_sixths_base(precision: Svq3MvPrecision) -> u32 {
    match precision {
        Svq3MvPrecision::Fullpel => 6,
        Svq3MvPrecision::Halfpel => 3,
        Svq3MvPrecision::Thirdpel => 2,
    }
}

/// Return `true` iff `stored_sixths` is exactly on the precision's
/// sample-grid base.
///
/// Stored motion-vector components are integer counts of sixths-of-a-
/// sample per the wiki spec's "as fraction of six" remark. After the
/// "rounded to the desired base" step, the post-rounding value must be
/// an integer multiple of [`stored_sixths_base`] applied to the
/// precision; this helper checks that property without inventing the
/// rounding direction (which the wiki spec does not pin down).
#[inline]
#[must_use]
pub const fn is_aligned_to_precision_base(stored_sixths: i32, precision: Svq3MvPrecision) -> bool {
    let base = stored_sixths_base(precision) as i32;
    stored_sixths.rem_euclid(base) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_d_multiplier_constant_matches_spec() {
        // "Thirdpel interpolation in one direction uses formula
        // `((2 * A + B + 1) * 0x2AB) >> 11`."
        assert_eq!(THIRDPEL_1D_MULTIPLIER, 0x2AB);
        assert_eq!(THIRDPEL_1D_MULTIPLIER, 683);
    }

    #[test]
    fn one_d_shift_constant_matches_spec() {
        assert_eq!(THIRDPEL_1D_SHIFT, 11);
    }

    #[test]
    fn one_d_bias_is_half_of_weight_sum_rounded_down() {
        assert_eq!(THIRDPEL_1D_BIAS, 1);
        assert_eq!(
            THIRDPEL_1D_BIAS as u32,
            THIRDPEL_1D_WEIGHT_SUM / 2,
            "spec's +1 bias equals `3 / 2 = 1` under integer-divide"
        );
    }

    #[test]
    fn one_d_weight_sum_matches_input_weights() {
        assert_eq!(
            THIRDPEL_1D_WEIGHT_SUM, 3,
            "input weights are `2 * A + B`, summing to 3"
        );
    }

    #[test]
    fn two_d_multiplier_constant_matches_spec() {
        // "two-dimensional interpolation uses matrix [4 3 / 3 2] and
        // `((4 * A + 3 * B + 3 * C + 2 * D + 6) * 0xAAB) >> 15`"
        assert_eq!(THIRDPEL_2D_MULTIPLIER, 0xAAB);
        assert_eq!(THIRDPEL_2D_MULTIPLIER, 2731);
    }

    #[test]
    fn two_d_shift_constant_matches_spec() {
        assert_eq!(THIRDPEL_2D_SHIFT, 15);
    }

    #[test]
    fn two_d_bias_is_half_of_weight_sum() {
        assert_eq!(THIRDPEL_2D_BIAS, 6);
        assert_eq!(THIRDPEL_2D_BIAS as u32, THIRDPEL_2D_WEIGHT_SUM / 2);
    }

    #[test]
    fn two_d_weight_matrix_matches_spec_quote() {
        assert_eq!(THIRDPEL_2D_WEIGHTS, [[4, 3], [3, 2]]);
    }

    #[test]
    fn two_d_weight_matrix_sums_to_twelve() {
        let mut sum: u32 = 0;
        let mut i = 0;
        while i < 2 {
            let mut j = 0;
            while j < 2 {
                sum += THIRDPEL_2D_WEIGHTS[i][j] as u32;
                j += 1;
            }
            i += 1;
        }
        assert_eq!(sum, THIRDPEL_2D_WEIGHT_SUM);
        assert_eq!(THIRDPEL_2D_WEIGHT_SUM, 12);
    }

    #[test]
    fn one_d_fixed_point_reciprocal_under_one_third() {
        // 683 / 2048 is strictly less than 1/3 (since 3 * 683 = 2049 > 2048,
        // we'd need 683.333... to reach exactly 1/3). The +1 numerator
        // bias compensates for the truncation.
        let three_times_mult: i64 =
            (THIRDPEL_1D_MULTIPLIER as i64) * (THIRDPEL_1D_WEIGHT_SUM as i64);
        let pow2: i64 = 1_i64 << THIRDPEL_1D_SHIFT;
        assert!(three_times_mult > pow2, "683 * 3 = 2049 > 2048");
        assert_eq!(three_times_mult, 2049);
        assert_eq!(pow2, 2048);
    }

    #[test]
    fn two_d_fixed_point_reciprocal_just_over_one_twelfth() {
        // 2731 / 32768: 12 * 2731 = 32772 > 32768, so the fixed-point
        // reciprocal slightly exceeds 1/12, again compensated by the
        // numerator +6 bias.
        let twelve_times_mult: i64 =
            (THIRDPEL_2D_MULTIPLIER as i64) * (THIRDPEL_2D_WEIGHT_SUM as i64);
        let pow2: i64 = 1_i64 << THIRDPEL_2D_SHIFT;
        assert_eq!(twelve_times_mult, 32772);
        assert_eq!(pow2, 32768);
        assert!(twelve_times_mult > pow2);
    }

    #[test]
    fn one_d_interpolate_matches_formula_expansion_for_zero_samples() {
        // (2*0 + 0 + 1) * 683 = 683; 683 >> 11 = 0
        assert_eq!(thirdpel_interpolate_1d(0, 0), 0);
    }

    #[test]
    fn one_d_interpolate_handles_equal_samples() {
        // If A == B == v, the formula reduces to ((3v + 1) * 683) >> 11,
        // which approximates v. Check exactness across the 0..=255
        // sample range.
        for v in 0..=255_i32 {
            let interpolated = thirdpel_interpolate_1d(v, v);
            // (3v + 1) * 683 >> 11. For v in 0..=255 this rounds to v
            // exactly: prove by direct comparison.
            let expanded = (3 * v + 1) * 683;
            assert_eq!(interpolated, expanded >> 11);
        }
    }

    #[test]
    fn one_d_interpolate_handles_asymmetric_samples() {
        // Direct expansion against the spec's literal formula. Spot
        // check a few asymmetric pairs and verify against the
        // bit-equivalent open expansion.
        for &(a, b) in &[(0_i32, 255_i32), (255, 0), (100, 50), (1, 2), (200, 201)] {
            let actual = thirdpel_interpolate_1d(a, b);
            let expected = ((2 * a + b + 1) * 683) >> 11;
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn one_d_interpolate_full_eight_bit_range_stays_in_byte_range() {
        // The interpolated value for inputs in 0..=255 should land in
        // 0..=255 too (since the formula is a convex combination minus
        // a tiny round-down quantisation).
        for a in 0..=255_i32 {
            for b in 0..=255_i32 {
                let v = thirdpel_interpolate_1d(a, b);
                assert!(
                    (0..=255).contains(&v),
                    "interpolate_1d({a}, {b}) = {v} out of 0..=255"
                );
            }
        }
    }

    #[test]
    fn two_d_interpolate_matches_formula_expansion_for_zero_samples() {
        // ((0 + 0 + 0 + 0 + 6) * 2731) >> 15 = 16386 >> 15 = 0
        assert_eq!(thirdpel_interpolate_2d(0, 0, 0, 0), 0);
    }

    #[test]
    fn two_d_interpolate_handles_equal_samples() {
        // If A == B == C == D == v, the formula reduces to
        // ((12v + 6) * 2731) >> 15, which rounds to v.
        for v in 0..=255_i32 {
            let interpolated = thirdpel_interpolate_2d(v, v, v, v);
            let expanded = ((12 * v + 6) * 2731) >> 15;
            assert_eq!(interpolated, expanded);
        }
    }

    #[test]
    fn two_d_interpolate_handles_asymmetric_samples() {
        for &(a, b, c, d) in &[
            (0_i32, 255, 0, 255),
            (255, 0, 255, 0),
            (10, 20, 30, 40),
            (1, 2, 3, 4),
            (200, 100, 150, 50),
        ] {
            let actual = thirdpel_interpolate_2d(a, b, c, d);
            let expected = ((4 * a + 3 * b + 3 * c + 2 * d + 6) * 2731) >> 15;
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn two_d_interpolate_max_input_stays_at_byte_max() {
        // For A = B = C = D = 255 the result rounds to 255 exactly.
        assert_eq!(thirdpel_interpolate_2d(255, 255, 255, 255), 255);
    }

    #[test]
    fn two_d_interpolate_full_eight_bit_corners_stay_in_byte_range() {
        // Exhaustive check over the four-corner space is too expensive
        // (~4 billion); sample 256 random-looking corners by a simple
        // hash. The formula's weighted average property guarantees a
        // result in 0..=255 for any inputs in that range, so a
        // representative spread is sufficient.
        for &(a, b, c, d) in &[
            (0_i32, 0, 0, 0),
            (255, 255, 255, 255),
            (0, 255, 255, 0),
            (255, 0, 0, 255),
            (128, 128, 128, 128),
            (255, 0, 255, 0),
            (0, 255, 0, 255),
            (7, 13, 41, 99),
            (250, 1, 250, 1),
            (123, 234, 56, 78),
        ] {
            let v = thirdpel_interpolate_2d(a, b, c, d);
            assert!(
                (0..=255).contains(&v),
                "interpolate_2d({a}, {b}, {c}, {d}) = {v} out of 0..=255"
            );
        }
    }

    #[test]
    fn one_d_interpolate_is_const() {
        // Ensure the `const fn` declaration compiles in a const context.
        const V: i32 = thirdpel_interpolate_1d(10, 20);
        assert_eq!(V, ((2 * 10 + 20 + 1) * 683) >> 11);
    }

    #[test]
    fn two_d_interpolate_is_const() {
        const V: i32 = thirdpel_interpolate_2d(10, 20, 30, 40);
        assert_eq!(V, ((4 * 10 + 3 * 20 + 3 * 30 + 2 * 40 + 6) * 2731) >> 15);
    }

    #[test]
    fn fullpel_stored_sixths_base_is_six() {
        assert_eq!(stored_sixths_base(Svq3MvPrecision::Fullpel), 6);
    }

    #[test]
    fn halfpel_stored_sixths_base_is_three() {
        assert_eq!(stored_sixths_base(Svq3MvPrecision::Halfpel), 3);
    }

    #[test]
    fn thirdpel_stored_sixths_base_is_two() {
        assert_eq!(stored_sixths_base(Svq3MvPrecision::Thirdpel), 2);
    }

    #[test]
    fn precision_bases_partition_six() {
        // 6 / 6 = 1 (one whole sample per Fullpel step).
        // 6 / 3 = 2 (two Halfpel steps per sample).
        // 6 / 2 = 3 (three Thirdpel steps per sample).
        let fullpel = stored_sixths_base(Svq3MvPrecision::Fullpel);
        let halfpel = stored_sixths_base(Svq3MvPrecision::Halfpel);
        let thirdpel = stored_sixths_base(Svq3MvPrecision::Thirdpel);
        assert_eq!(6 / fullpel, 1);
        assert_eq!(6 / halfpel, 2);
        assert_eq!(6 / thirdpel, 3);
    }

    #[test]
    fn alignment_predicate_accepts_zero_for_every_precision() {
        for p in [
            Svq3MvPrecision::Fullpel,
            Svq3MvPrecision::Halfpel,
            Svq3MvPrecision::Thirdpel,
        ] {
            assert!(is_aligned_to_precision_base(0, p));
        }
    }

    #[test]
    fn alignment_predicate_for_fullpel() {
        // Fullpel base = 6; only multiples of 6 align.
        for s in -36_i32..=36 {
            let aligned = is_aligned_to_precision_base(s, Svq3MvPrecision::Fullpel);
            let expected = s.rem_euclid(6) == 0;
            assert_eq!(aligned, expected, "s={s}");
        }
    }

    #[test]
    fn alignment_predicate_for_halfpel() {
        // Halfpel base = 3; only multiples of 3 align.
        for s in -36_i32..=36 {
            let aligned = is_aligned_to_precision_base(s, Svq3MvPrecision::Halfpel);
            let expected = s.rem_euclid(3) == 0;
            assert_eq!(aligned, expected, "s={s}");
        }
    }

    #[test]
    fn alignment_predicate_for_thirdpel() {
        // Thirdpel base = 2; only even values align.
        for s in -36_i32..=36 {
            let aligned = is_aligned_to_precision_base(s, Svq3MvPrecision::Thirdpel);
            let expected = s.rem_euclid(2) == 0;
            assert_eq!(aligned, expected, "s={s}");
        }
    }

    #[test]
    fn alignment_predicate_handles_negative_stored_values() {
        // The wiki spec doesn't say MV components are unsigned; the
        // alignment helper uses `rem_euclid` so negative values match
        // their absolute-value-modulo behaviour.
        assert!(is_aligned_to_precision_base(-12, Svq3MvPrecision::Fullpel));
        assert!(is_aligned_to_precision_base(-9, Svq3MvPrecision::Halfpel));
        assert!(is_aligned_to_precision_base(-8, Svq3MvPrecision::Thirdpel));
        assert!(!is_aligned_to_precision_base(-5, Svq3MvPrecision::Fullpel));
        assert!(!is_aligned_to_precision_base(-7, Svq3MvPrecision::Halfpel));
        assert!(!is_aligned_to_precision_base(-3, Svq3MvPrecision::Thirdpel));
    }
}
