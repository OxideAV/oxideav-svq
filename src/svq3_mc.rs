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
//! ## Reference-frame fetch
//!
//! The full-pel reference copy that the sub-pel filters refine lands as
//! [`ReferencePlane`] (a borrowed row-major picture-plane view with
//! H.264 edge-replication clamping for unrestricted motion vectors) plus
//! [`fetch_fullpel_block`] (a `block_w × block_h` clamped integer-pel
//! copy). When a motion vector lands exactly on the integer grid this
//! copy *is* the macroblock predictor; for sub-pel vectors it is the
//! pre-filter window the thirdpel interpolators sharpen.
//!
//! ## Open work
//!
//! The full sub-pel filter-application stage (selecting 1-D vs 2-D
//! thirdpel per fractional grid position) and the precision rounding of
//! the stored sixths grid into a fetch offset are not yet wired — the
//! wiki leaves the rounding direction and the per-position filter
//! selection unpinned (a deferred DOCS-GAP).
//! `Svq3DecoderHandle::receive_frame` continues to return
//! `oxideav_core::Error::Unsupported`.

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

/// A borrowed read-only view of a reference picture plane (luma or one
/// chroma component) used as the source for motion compensation.
///
/// The plane is row-major, `width × height` samples, indexed
/// `samples[y * width + x]`. Motion compensation reads a rectangular
/// window out of this plane at an integer-pel offset that may fall
/// partly (or wholly) outside the plane bounds; out-of-bounds
/// coordinates are resolved by **edge replication** (clamping each
/// coordinate to the nearest in-bounds sample), the standard H.264
/// reference-sample border extension that
/// `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 5 refers
/// to as the surrounding `Clip1` / saturation idiom for the
/// reconstruction path. SVQ3 inherits H.264's unrestricted motion
/// vectors, so the reference window is never bounds-checked against the
/// frame — it is clamped.
#[derive(Debug, Clone, Copy)]
pub struct ReferencePlane<'a> {
    samples: &'a [u8],
    width: usize,
    height: usize,
}

impl<'a> ReferencePlane<'a> {
    /// Wrap a row-major sample slice as a `width × height` reference
    /// plane.
    ///
    /// Returns `None` if `samples.len() != width * height` or if either
    /// dimension is zero (an empty plane has no sample to clamp to).
    #[must_use]
    pub fn new(samples: &'a [u8], width: usize, height: usize) -> Option<Self> {
        if width == 0 || height == 0 || samples.len() != width * height {
            return None;
        }
        Some(Self {
            samples,
            width,
            height,
        })
    }

    /// Plane width in samples.
    #[inline]
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Plane height in samples.
    #[inline]
    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Fetch the sample at `(x, y)` with edge-replication clamping.
    ///
    /// `x` and `y` are signed integer-pel coordinates that may be
    /// negative or beyond the plane extent; each is clamped to
    /// `[0, width-1]` / `[0, height-1]` before the row-major lookup, so
    /// this never panics and never returns out-of-band data. This is the
    /// standard H.264 border extension for unrestricted motion vectors.
    #[inline]
    #[must_use]
    pub fn sample_clamped(&self, x: i32, y: i32) -> u8 {
        let cx = x.clamp(0, self.width as i32 - 1) as usize;
        let cy = y.clamp(0, self.height as i32 - 1) as usize;
        self.samples[cy * self.width + cx]
    }
}

/// Fetch a `block_w × block_h` integer-pel block from `plane` whose
/// top-left sample is at plane coordinate `(origin_x, origin_y)`, with
/// out-of-bounds samples resolved by edge replication.
///
/// This is the **full-pel** motion-compensation copy: when a motion
/// vector lands exactly on the integer-pel grid (no sub-pel fraction),
/// the predictor for the block is just this clamped reference copy. The
/// output is row-major (`out[row * block_w + col]`), matching the layout
/// the 4×4 / 16×16 reconstruction loops consume so the residual can be
/// added element-wise.
///
/// `origin_x` / `origin_y` are signed (they incorporate the block's
/// pixel position plus the integer-pel part of its motion vector and may
/// be negative); each fetched sample is clamped per
/// [`ReferencePlane::sample_clamped`].
#[must_use]
pub fn fetch_fullpel_block(
    plane: &ReferencePlane<'_>,
    origin_x: i32,
    origin_y: i32,
    block_w: usize,
    block_h: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(block_w * block_h);
    for row in 0..block_h {
        let sy = origin_y + row as i32;
        for col in 0..block_w {
            let sx = origin_x + col as i32;
            out.push(plane.sample_clamped(sx, sy));
        }
    }
    out
}

/// The number of sub-sample subdivisions a stored motion-vector
/// component is expressed in: sixths-of-a-sample.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion
/// Compensation": *"motion vectors are stored and predicted as fraction
/// of six"*. The common storage grid is sixths so each of the three
/// supported precisions (whole = 6/6, half = 3/6, third = 2/6) lands on
/// an integer sixths value.
pub const MV_FRACTION_BASE: i32 = 6;

/// A stored motion-vector component split into its integer-pel part and
/// its sub-pel remainder on the sixths grid.
///
/// `integer_pel` is the whole-sample displacement (the offset added to a
/// block's pixel position to locate the [`fetch_fullpel_block`] window
/// origin); `frac_sixths` is the residual sub-sample offset in
/// `0..MV_FRACTION_BASE` (always non-negative), the input to the sub-pel
/// interpolation filters. When `frac_sixths == 0` the component lands
/// exactly on the integer grid and needs no interpolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MvComponentSplit {
    /// Whole-sample displacement (signed, floored toward −∞).
    pub integer_pel: i32,
    /// Sub-sample remainder in sixths, `0..6` (always non-negative).
    pub frac_sixths: u32,
}

/// Split one stored motion-vector component (in sixths-of-a-sample)
/// into its integer-pel displacement and its non-negative sub-pel
/// remainder.
///
/// The split is the Euclidean division of the stored sixths value by
/// [`MV_FRACTION_BASE`]: `integer_pel = ⌊stored_sixths / 6⌋` (floored
/// toward −∞ so a negative vector still has a non-negative fractional
/// remainder) and `frac_sixths = stored_sixths mod 6 ∈ 0..6`. Flooring
/// toward −∞ keeps the fractional position consistent regardless of
/// sign: a component of `−1` sixths is `−1` whole pels plus `+5` sixths,
/// i.e. one sample left then five-sixths right, the same sub-pel phase a
/// `+5` component carries. This reconstructs the canonical
/// `position = integer_pel + frac_sixths/6` for every signed input.
#[inline]
#[must_use]
pub const fn split_mv_component(stored_sixths: i32) -> MvComponentSplit {
    MvComponentSplit {
        integer_pel: stored_sixths.div_euclid(MV_FRACTION_BASE),
        frac_sixths: stored_sixths.rem_euclid(MV_FRACTION_BASE) as u32,
    }
}

/// Clip an interpolated `i32` sample to the 8-bit pixel range
/// `[0, 255]`.
///
/// The per-sample thirdpel formulas ([`thirdpel_interpolate_1d`] /
/// [`thirdpel_interpolate_2d`]) return an `i32` that is a rounded convex
/// combination of in-range samples and therefore already lands in
/// `0..=255` for in-range inputs; this `Clip1` is the defensive
/// saturation the reconstruction path applies
/// (`docs/video/svq3/spec/01-reconstruction-composition.md` Gap 5).
#[inline]
#[must_use]
const fn clip1_u8(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v as u8
    }
}

/// Interpolate a `block_w × block_h` block whose integer-pel origin is
/// `(origin_x, origin_y)`, sharpened **horizontally** by the
/// one-dimensional thirdpel filter.
///
/// Each output sample is `thirdpel_interpolate_1d(A, B)` where `A` is
/// the integer-pel sample at the block position (the nearer sample,
/// weight 2) and `B` is the sample one full pel to the **right** (the
/// farther sample, weight 1), both fetched from `plane` with
/// [`ReferencePlane::sample_clamped`] edge handling. This realises the
/// wiki's `((2·A + B + 1)·0x2AB) >> 11` for a horizontal sub-pel offset.
/// The caller positions `origin_*` so `A` is the nearer integer sample
/// (via [`split_mv_component`]'s `integer_pel`); the fractional phase
/// the wiki's "fraction of six … rounded to base" step selects is not
/// pinned, so this helper covers the single forward-neighbour 1-D case
/// the formula spells out. Output is row-major, `Clip1`-saturated to
/// `0..=255`.
#[must_use]
pub fn interpolate_block_thirdpel_h(
    plane: &ReferencePlane<'_>,
    origin_x: i32,
    origin_y: i32,
    block_w: usize,
    block_h: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(block_w * block_h);
    for row in 0..block_h {
        let sy = origin_y + row as i32;
        for col in 0..block_w {
            let sx = origin_x + col as i32;
            let a = plane.sample_clamped(sx, sy) as i32;
            let b = plane.sample_clamped(sx + 1, sy) as i32;
            out.push(clip1_u8(thirdpel_interpolate_1d(a, b)));
        }
    }
    out
}

/// Interpolate a `block_w × block_h` block sharpened **vertically** by
/// the one-dimensional thirdpel filter.
///
/// Identical to [`interpolate_block_thirdpel_h`] but `B` is the sample
/// one full pel **below** each position (weight 1) and `A` the sample at
/// the position (weight 2): `thirdpel_interpolate_1d(A, B)` for a
/// vertical sub-pel offset. Output is row-major, `Clip1`-saturated.
#[must_use]
pub fn interpolate_block_thirdpel_v(
    plane: &ReferencePlane<'_>,
    origin_x: i32,
    origin_y: i32,
    block_w: usize,
    block_h: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(block_w * block_h);
    for row in 0..block_h {
        let sy = origin_y + row as i32;
        for col in 0..block_w {
            let sx = origin_x + col as i32;
            let a = plane.sample_clamped(sx, sy) as i32;
            let b = plane.sample_clamped(sx, sy + 1) as i32;
            out.push(clip1_u8(thirdpel_interpolate_1d(a, b)));
        }
    }
    out
}

/// Interpolate a `block_w × block_h` block sharpened in **both**
/// directions by the two-dimensional thirdpel filter.
///
/// Each output sample is `thirdpel_interpolate_2d(A, B, C, D)` where the
/// four inputs are the 2×2 integer-pel neighbourhood at the block
/// position in the [`THIRDPEL_2D_WEIGHTS`] row-major order: `A` at
/// `(x, y)` (weight 4), `B` at `(x+1, y)` (weight 3), `C` at `(x, y+1)`
/// (weight 3), `D` at `(x+1, y+1)` (weight 2). This realises the wiki's
/// `((4·A + 3·B + 3·C + 2·D + 6)·0xAAB) >> 15` across a whole block.
/// Output is row-major, `Clip1`-saturated to `0..=255`.
#[must_use]
pub fn interpolate_block_thirdpel_2d(
    plane: &ReferencePlane<'_>,
    origin_x: i32,
    origin_y: i32,
    block_w: usize,
    block_h: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(block_w * block_h);
    for row in 0..block_h {
        let sy = origin_y + row as i32;
        for col in 0..block_w {
            let sx = origin_x + col as i32;
            let a = plane.sample_clamped(sx, sy) as i32;
            let b = plane.sample_clamped(sx + 1, sy) as i32;
            let c = plane.sample_clamped(sx, sy + 1) as i32;
            let d = plane.sample_clamped(sx + 1, sy + 1) as i32;
            out.push(clip1_u8(thirdpel_interpolate_2d(a, b, c, d)));
        }
    }
    out
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

    // ------------------------------------------------------------------
    // Reference-plane integer-pel block fetch (Milestone 1)
    // ------------------------------------------------------------------

    /// Build a `width × height` ramp plane where `samples[y*w+x] =
    /// (y*w + x) % 256`, useful for verifying coordinate addressing.
    fn ramp_plane(width: usize, height: usize) -> Vec<u8> {
        (0..width * height).map(|i| (i % 256) as u8).collect()
    }

    #[test]
    fn reference_plane_rejects_mismatched_length() {
        assert!(ReferencePlane::new(&[0u8; 5], 2, 3).is_none());
        assert!(ReferencePlane::new(&[0u8; 6], 2, 3).is_some());
    }

    #[test]
    fn reference_plane_rejects_zero_dimension() {
        assert!(ReferencePlane::new(&[], 0, 3).is_none());
        assert!(ReferencePlane::new(&[], 2, 0).is_none());
    }

    #[test]
    fn reference_plane_reports_dimensions() {
        let buf = ramp_plane(4, 5);
        let plane = ReferencePlane::new(&buf, 4, 5).unwrap();
        assert_eq!(plane.width(), 4);
        assert_eq!(plane.height(), 5);
    }

    #[test]
    fn sample_clamped_in_bounds_is_row_major() {
        let buf = ramp_plane(4, 3);
        let plane = ReferencePlane::new(&buf, 4, 3).unwrap();
        // samples[y*4 + x]
        assert_eq!(plane.sample_clamped(0, 0), 0);
        assert_eq!(plane.sample_clamped(3, 0), 3);
        assert_eq!(plane.sample_clamped(0, 1), 4);
        assert_eq!(plane.sample_clamped(2, 2), 10);
    }

    #[test]
    fn sample_clamped_clamps_negative_coordinates_to_edge() {
        let buf = ramp_plane(4, 3);
        let plane = ReferencePlane::new(&buf, 4, 3).unwrap();
        // x<0 → column 0; y<0 → row 0.
        assert_eq!(plane.sample_clamped(-5, 0), plane.sample_clamped(0, 0));
        assert_eq!(plane.sample_clamped(2, -9), plane.sample_clamped(2, 0));
        assert_eq!(plane.sample_clamped(-1, -1), plane.sample_clamped(0, 0));
    }

    #[test]
    fn sample_clamped_clamps_beyond_extent_to_edge() {
        let buf = ramp_plane(4, 3);
        let plane = ReferencePlane::new(&buf, 4, 3).unwrap();
        // x>=w → last column; y>=h → last row.
        assert_eq!(plane.sample_clamped(99, 1), plane.sample_clamped(3, 1));
        assert_eq!(plane.sample_clamped(2, 99), plane.sample_clamped(2, 2));
        assert_eq!(plane.sample_clamped(99, 99), plane.sample_clamped(3, 2));
    }

    #[test]
    fn fetch_fullpel_block_copies_interior_window() {
        let buf = ramp_plane(8, 8);
        let plane = ReferencePlane::new(&buf, 8, 8).unwrap();
        let block = fetch_fullpel_block(&plane, 2, 3, 4, 4);
        assert_eq!(block.len(), 16);
        // Each output sample equals the clamped reference sample.
        for row in 0..4 {
            for col in 0..4 {
                let got = block[row * 4 + col];
                let want = plane.sample_clamped(2 + col as i32, 3 + row as i32);
                assert_eq!(got, want, "row={row} col={col}");
            }
        }
        // The interior window is unclamped: top-left = samples[3*8+2] = 26.
        assert_eq!(block[0], 26);
    }

    #[test]
    fn fetch_fullpel_block_replicates_top_left_corner() {
        let buf = ramp_plane(8, 8);
        let plane = ReferencePlane::new(&buf, 8, 8).unwrap();
        // Origin entirely above-left of the plane: the whole 4×4 block
        // replicates the single corner sample samples[0] = 0.
        let block = fetch_fullpel_block(&plane, -10, -10, 4, 4);
        assert!(block.iter().all(|&s| s == 0), "{block:?}");
    }

    #[test]
    fn fetch_fullpel_block_replicates_bottom_right_corner() {
        let buf = ramp_plane(8, 8);
        let plane = ReferencePlane::new(&buf, 8, 8).unwrap();
        let corner = plane.sample_clamped(7, 7);
        // Origin entirely below-right: every sample replicates the
        // bottom-right corner.
        let block = fetch_fullpel_block(&plane, 20, 20, 4, 4);
        assert!(block.iter().all(|&s| s == corner), "{block:?}");
    }

    #[test]
    fn fetch_fullpel_block_handles_partial_overlap() {
        let buf = ramp_plane(8, 8);
        let plane = ReferencePlane::new(&buf, 8, 8).unwrap();
        // Origin straddling the left edge: columns left of 0 replicate
        // column 0, in-bounds columns copy verbatim.
        let block = fetch_fullpel_block(&plane, -2, 1, 4, 1);
        // col 0,1 → x clamps to 0; col 2 → x=0; col 3 → x=1.
        assert_eq!(block[0], plane.sample_clamped(0, 1));
        assert_eq!(block[1], plane.sample_clamped(0, 1));
        assert_eq!(block[2], plane.sample_clamped(0, 1));
        assert_eq!(block[3], plane.sample_clamped(1, 1));
    }

    #[test]
    fn fetch_fullpel_block_full_16x16_size() {
        let buf = ramp_plane(32, 32);
        let plane = ReferencePlane::new(&buf, 32, 32).unwrap();
        let block = fetch_fullpel_block(&plane, 5, 7, 16, 16);
        assert_eq!(block.len(), 256);
        assert_eq!(block[0], plane.sample_clamped(5, 7));
        assert_eq!(block[16 * 16 - 1], plane.sample_clamped(5 + 15, 7 + 15));
    }

    // ------------------------------------------------------------------
    // MV sixths-grid decomposition (Milestone 2)
    // ------------------------------------------------------------------

    #[test]
    fn mv_fraction_base_is_six() {
        assert_eq!(MV_FRACTION_BASE, 6);
    }

    #[test]
    fn split_mv_component_zero() {
        let s = split_mv_component(0);
        assert_eq!(s.integer_pel, 0);
        assert_eq!(s.frac_sixths, 0);
    }

    #[test]
    fn split_mv_component_exact_full_pels() {
        // Multiples of six are whole pels with zero fraction.
        for pel in -5..=5 {
            let s = split_mv_component(pel * 6);
            assert_eq!(s.integer_pel, pel, "pel={pel}");
            assert_eq!(s.frac_sixths, 0, "pel={pel}");
        }
    }

    #[test]
    fn split_mv_component_positive_fractions() {
        // 7 sixths = 1 whole pel + 1 sixth.
        let s = split_mv_component(7);
        assert_eq!(s.integer_pel, 1);
        assert_eq!(s.frac_sixths, 1);
        // 3 sixths = 0 whole + 3 sixths (a halfpel phase).
        let h = split_mv_component(3);
        assert_eq!(h.integer_pel, 0);
        assert_eq!(h.frac_sixths, 3);
        // 4 sixths = 0 whole + 4 sixths (a two-thirds phase).
        let t = split_mv_component(4);
        assert_eq!(t.integer_pel, 0);
        assert_eq!(t.frac_sixths, 4);
    }

    #[test]
    fn split_mv_component_negative_floors_toward_neg_inf() {
        // -1 sixth = -1 whole pel + 5 sixths.
        let s = split_mv_component(-1);
        assert_eq!(s.integer_pel, -1);
        assert_eq!(s.frac_sixths, 5);
        // -6 sixths = -1 whole pel + 0.
        let m = split_mv_component(-6);
        assert_eq!(m.integer_pel, -1);
        assert_eq!(m.frac_sixths, 0);
        // -7 sixths = -2 whole pels + 5 sixths.
        let n = split_mv_component(-7);
        assert_eq!(n.integer_pel, -2);
        assert_eq!(n.frac_sixths, 5);
    }

    #[test]
    fn split_mv_component_reconstructs_position_for_every_input() {
        // integer_pel * 6 + frac_sixths == stored_sixths for all inputs.
        for v in -100i32..=100 {
            let s = split_mv_component(v);
            assert!(s.frac_sixths < MV_FRACTION_BASE as u32, "v={v}");
            assert_eq!(
                s.integer_pel * MV_FRACTION_BASE + s.frac_sixths as i32,
                v,
                "v={v}"
            );
        }
    }

    #[test]
    fn split_mv_component_is_const() {
        const S: MvComponentSplit = split_mv_component(-7);
        assert_eq!(S.integer_pel, -2);
        assert_eq!(S.frac_sixths, 5);
    }

    // ------------------------------------------------------------------
    // Whole-block thirdpel interpolation (Milestone 3)
    // ------------------------------------------------------------------

    #[test]
    fn interpolate_h_on_uniform_plane_is_identity() {
        // A flat plane interpolates to the same value (the filter is a
        // weighted average of equal samples).
        let buf = vec![100u8; 8 * 8];
        let plane = ReferencePlane::new(&buf, 8, 8).unwrap();
        let block = interpolate_block_thirdpel_h(&plane, 1, 1, 4, 4);
        assert!(block.iter().all(|&s| s == 100), "{block:?}");
    }

    #[test]
    fn interpolate_v_and_2d_on_uniform_plane_are_identity() {
        let buf = vec![77u8; 8 * 8];
        let plane = ReferencePlane::new(&buf, 8, 8).unwrap();
        assert!(interpolate_block_thirdpel_v(&plane, 1, 1, 4, 4)
            .iter()
            .all(|&s| s == 77));
        assert!(interpolate_block_thirdpel_2d(&plane, 1, 1, 4, 4)
            .iter()
            .all(|&s| s == 77));
    }

    #[test]
    fn interpolate_h_matches_per_sample_formula() {
        let buf = ramp_plane(8, 8);
        let plane = ReferencePlane::new(&buf, 8, 8).unwrap();
        let block = interpolate_block_thirdpel_h(&plane, 2, 3, 4, 4);
        for row in 0..4 {
            for col in 0..4 {
                let sx = 2 + col as i32;
                let sy = 3 + row as i32;
                let a = plane.sample_clamped(sx, sy) as i32;
                let b = plane.sample_clamped(sx + 1, sy) as i32;
                let want = clip1_u8(thirdpel_interpolate_1d(a, b));
                assert_eq!(block[row * 4 + col], want, "row={row} col={col}");
            }
        }
    }

    #[test]
    fn interpolate_v_matches_per_sample_formula() {
        let buf = ramp_plane(8, 8);
        let plane = ReferencePlane::new(&buf, 8, 8).unwrap();
        let block = interpolate_block_thirdpel_v(&plane, 1, 1, 3, 5);
        for row in 0..5 {
            for col in 0..3 {
                let sx = 1 + col as i32;
                let sy = 1 + row as i32;
                let a = plane.sample_clamped(sx, sy) as i32;
                let b = plane.sample_clamped(sx, sy + 1) as i32;
                let want = clip1_u8(thirdpel_interpolate_1d(a, b));
                assert_eq!(block[row * 3 + col], want, "row={row} col={col}");
            }
        }
    }

    #[test]
    fn interpolate_2d_matches_per_sample_formula() {
        let buf = ramp_plane(8, 8);
        let plane = ReferencePlane::new(&buf, 8, 8).unwrap();
        let block = interpolate_block_thirdpel_2d(&plane, 2, 2, 4, 4);
        for row in 0..4 {
            for col in 0..4 {
                let sx = 2 + col as i32;
                let sy = 2 + row as i32;
                let a = plane.sample_clamped(sx, sy) as i32;
                let b = plane.sample_clamped(sx + 1, sy) as i32;
                let c = plane.sample_clamped(sx, sy + 1) as i32;
                let d = plane.sample_clamped(sx + 1, sy + 1) as i32;
                let want = clip1_u8(thirdpel_interpolate_2d(a, b, c, d));
                assert_eq!(block[row * 4 + col], want, "row={row} col={col}");
            }
        }
    }

    #[test]
    fn interpolate_clips_to_byte_range() {
        // All interpolators must stay within 0..=255 for in-range input.
        let buf = ramp_plane(16, 16);
        let plane = ReferencePlane::new(&buf, 16, 16).unwrap();
        for b in [
            interpolate_block_thirdpel_h(&plane, 0, 0, 16, 16),
            interpolate_block_thirdpel_v(&plane, 0, 0, 16, 16),
            interpolate_block_thirdpel_2d(&plane, 0, 0, 16, 16),
        ] {
            assert_eq!(b.len(), 256);
            // u8 output is inherently in range; assert the count to
            // guard against silent truncation.
        }
    }

    #[test]
    fn interpolate_h_edge_clamps_at_right_border() {
        // At the rightmost column, B clamps to A (same column), so the
        // 1-D filter reduces to interpolating A with itself.
        let buf = ramp_plane(4, 4);
        let plane = ReferencePlane::new(&buf, 4, 4).unwrap();
        let block = interpolate_block_thirdpel_h(&plane, 3, 0, 1, 1);
        let a = plane.sample_clamped(3, 0) as i32;
        // B = sample_clamped(4,0) clamps to (3,0) == a.
        assert_eq!(block[0], clip1_u8(thirdpel_interpolate_1d(a, a)));
    }

    #[test]
    fn clip1_saturates_both_ends() {
        assert_eq!(clip1_u8(-1), 0);
        assert_eq!(clip1_u8(0), 0);
        assert_eq!(clip1_u8(128), 128);
        assert_eq!(clip1_u8(255), 255);
        assert_eq!(clip1_u8(300), 255);
    }
}
