//! SVQ3 macroblock transform and dequantization arithmetic
//! (structural).
//!
//! ## Provenance
//!
//! Round 230 lands the per-coefficient dequantization arithmetic
//! described in `docs/video/svq3/wiki/Sorenson_Video_3.wiki`
//! §"Macroblock transform and dequantization" (verbatim local mirror
//! of the multimedia.cx `Sorenson_Video_3` wiki page). The section
//! enumerates four pieces of data the macroblock pipeline needs:
//!
//! * The 4×4 luma transform coefficient matrix:
//!
//!   ```text
//!     13  17   1   7
//!     13   7  -1 -17
//!     13  -7  -1  17
//!     13 -17   1  -7
//!   ```
//!
//! * The 2×2 chroma DC transform matrix the chroma DCs are first
//!   transformed with:
//!
//!   ```text
//!     8  8
//!     8 -8
//!   ```
//!
//! * A 32-entry quantizer table indexed by the slice quantiser `Q ∈
//!   0..32` — exposed as [`DEQUANT_COEFF_TABLE`].
//!
//! * Three dequantization closed-form expressions:
//!     * For intra luma blocks without separate DC coefficients:
//!       `dc = 13 * 13 * 1538 * block[0]`
//!     * For chroma blocks:
//!       `dc = (DEQUANT_COEFF_TABLE[Q] * (block[0] >> 3)) >> 1`
//!     * General per-coefficient dequant:
//!       `out = (coeff * DEQUANT_COEFF_TABLE[Q] + dc + 0x80000) >> 20`
//!       where `dc = 0` if not defined otherwise.
//!
//! All four pieces of data land here as `pub const` arrays / scalars
//! plus three `const fn` helpers that apply the three closed-form
//! expressions verbatim. The constants are surfaced with neutral
//! identifiers ([`DEQUANT_COEFF_TABLE`], [`INTRA_LUMA_DC_SCALE`],
//! [`DEQUANT_ROUND`], [`DEQUANT_SHIFT`], [`LUMA_TRANSFORM_MATRIX`],
//! [`CHROMA_DC_TRANSFORM_MATRIX`]) so the spec's identifier choices
//! stay descriptive in the wiki without leaking into our public
//! surface.
//!
//! ## Numerical interpretation (informative)
//!
//! Round 230 does NOT mirror this as a spec claim — the wiki spec
//! simply lists the formulas — but `1538` in the intra-luma-DC scale
//! is suggestive of a fixed-point reciprocal derivation. The spec
//! does not derive it; round 230 transcribes the constant verbatim
//! and leaves the closed-form rationale to consumers.
//!
//! The general dequantization shift of `20` combined with the `+
//! 0x80000` (`= 1 << 19`) additive bias is the standard
//! `(x + 2^{n-1}) >> n` round-half-up step.
//!
//! ## Open work
//!
//! Round 230 lands the four data tables and the three closed-form
//! helpers; it does NOT wire them into a residual-decode pipeline yet.
//! The dezigzag stage that places the per-block
//! [`crate::svq3_coeff::Coefficient`] stream into a 4×4 grid and the
//! IDCT that consumes the dequantized output remain out of scope —
//! `Svq3DecoderHandle::receive_frame` continues to return
//! `oxideav_core::Error::Unsupported`.

use core::ops::Range;

/// The 4×4 luma transform coefficient matrix the wiki spec enumerates
/// verbatim under §"Macroblock transform and dequantization":
///
/// ```text
///   13  17   1   7
///   13   7  -1 -17
///   13  -7  -1  17
///   13 -17   1  -7
/// ```
///
/// Indexed `[row][col]` so `LUMA_TRANSFORM_MATRIX[0][0] = 13`,
/// `LUMA_TRANSFORM_MATRIX[3][3] = -7`. The four rows share the
/// constant column-0 value `13` — see [`LUMA_TRANSFORM_DC_COLUMN`].
pub const LUMA_TRANSFORM_MATRIX: [[i32; 4]; 4] = [
    [13, 17, 1, 7],
    [13, 7, -1, -17],
    [13, -7, -1, 17],
    [13, -17, 1, -7],
];

/// The column-0 value of [`LUMA_TRANSFORM_MATRIX`].
///
/// All four rows of the luma transform matrix have `13` in their
/// first column. This constant is exposed for compile-time
/// corroboration of that invariant; the intra-luma-DC scale
/// [`INTRA_LUMA_DC_SCALE`] folds it in as `13 * 13 * 1538`.
pub const LUMA_TRANSFORM_DC_COLUMN: i32 = 13;

/// The 2×2 chroma DC transform matrix the wiki spec quotes verbatim:
///
/// ```text
///   8  8
///   8 -8
/// ```
///
/// Per the spec, "chroma DCs need to be transformed first using the
/// following matrix" before the dequantization formula for chroma
/// blocks ([`dequantize_chroma_dc`]) is applied.
pub const CHROMA_DC_TRANSFORM_MATRIX: [[i32; 2]; 2] = [[8, 8], [8, -8]];

/// Number of entries in [`DEQUANT_COEFF_TABLE`].
///
/// The wiki spec's table is exactly 32 entries indexed by the slice
/// quantiser value `Q`. The slice header parser ([`crate::svq3`])
/// reads `Q` as a 5-bit field, so the valid range `0..=31` covers
/// every entry.
pub const DEQUANT_COEFF_TABLE_LEN: usize = 32;

/// Range of valid quantiser indices into [`DEQUANT_COEFF_TABLE`].
///
/// The slice header carries a 5-bit `slice_quantiser` value; this
/// range expresses the closed inclusive bound `0..=31`.
pub const DEQUANT_QUANTISER_RANGE: Range<u32> = 0..(DEQUANT_COEFF_TABLE_LEN as u32);

/// 32-entry dequantization coefficient table indexed by the slice
/// quantiser `Q`. Transcribed verbatim from
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock transform
/// and dequantization":
///
/// ```text
///   3881,  4351,  4890,  5481,  6154,  6914,  7761,  8718,
///   9781, 10987, 12339, 13828, 15523, 17435, 19561, 21873,
///  24552, 27656, 30847, 34870, 38807, 43747, 49103, 54683,
///  61694, 68745, 77615, 89113,100253,109366,126635,141533
/// ```
///
/// The values are strictly monotonically increasing across the full
/// 32-entry range (verified by a unit test below). The general
/// dequant formula `out = (coeff * DEQUANT_COEFF_TABLE[Q] + dc +
/// 0x80000) >> 20` from the same wiki section is exposed via
/// [`dequantize_coefficient`].
pub const DEQUANT_COEFF_TABLE: [u32; DEQUANT_COEFF_TABLE_LEN] = [
    3881, 4351, 4890, 5481, 6154, 6914, 7761, 8718, 9781, 10987, 12339, 13828, 15523, 17435, 19561,
    21873, 24552, 27656, 30847, 34870, 38807, 43747, 49103, 54683, 61694, 68745, 77615, 89113,
    100253, 109366, 126635, 141533,
];

/// The intra-luma-DC scale constant from the wiki spec's "For intra
/// luma blocks without separate DC coefficients block" expression
/// `dc = 13 * 13 * 1538 * block[0]`.
///
/// Folds the two transform column-0 values (`13 * 13 = 169`) with the
/// spec's `1538` standalone constant into a single multiplier
/// `260_322`. The [`dequantize_intra_luma_dc`] helper applies this
/// scale to its single argument and returns the resulting DC value
/// (with the standard `(x + DEQUANT_ROUND) >> DEQUANT_SHIFT` rounding
/// applied by the caller via [`finalise_dc`]).
pub const INTRA_LUMA_DC_SCALE: i32 = 13 * 13 * 1538;

/// The shift the wiki spec's general dequantization formula applies
/// after multiplying by [`DEQUANT_COEFF_TABLE`]: `>> 20`.
///
/// Used together with [`DEQUANT_ROUND`] to perform the standard
/// `(x + 2^{n-1}) >> n` round-half-up step.
pub const DEQUANT_SHIFT: u32 = 20;

/// The additive bias the wiki spec's general dequantization formula
/// adds before the [`DEQUANT_SHIFT`] right-shift: `+ 0x80000`.
///
/// Equals `1 << (DEQUANT_SHIFT - 1) = 0x80000 = 524_288` — the
/// standard round-half-up bias for the trailing right-shift by 20.
pub const DEQUANT_ROUND: i32 = 0x80000;

/// The standalone `1538` factor the wiki spec uses in the
/// intra-luma-DC expression `dc = 13 * 13 * 1538 * block[0]`.
///
/// Surfaced as its own constant for compile-time corroboration of
/// the [`INTRA_LUMA_DC_SCALE`] decomposition
/// (`INTRA_LUMA_DC_SCALE == LUMA_TRANSFORM_DC_COLUMN * LUMA_TRANSFORM_DC_COLUMN
/// * INTRA_LUMA_DC_SCALE_TAIL`).
pub const INTRA_LUMA_DC_SCALE_TAIL: i32 = 1538;

/// The chroma-DC pre-multiply shift the wiki spec's chroma expression
/// `dc = (DEQUANT_COEFF_TABLE[Q] * (block[0] >> 3)) >> 1` applies
/// before its trailing right-shift.
///
/// Equal to `3`. Combined with [`CHROMA_DC_POST_SHIFT`] the total
/// `>> 4` shift moves the chroma DC into the same fixed-point
/// register the general dequant formula consumes.
pub const CHROMA_DC_PRE_SHIFT: u32 = 3;

/// The chroma-DC post-multiply shift the wiki spec's chroma
/// expression applies after the multiplication.
///
/// Equal to `1`. Combined with [`CHROMA_DC_PRE_SHIFT`] the total
/// shift balances the `8 8 / 8 -8` chroma transform's pre-scaling.
pub const CHROMA_DC_POST_SHIFT: u32 = 1;

/// Apply the wiki spec's intra-luma DC expression `dc = 13 * 13 *
/// 1538 * block[0]` to the single argument `block_zero`.
///
/// The wiki spec uses this expression "for intra luma blocks without
/// separate DC coefficients block" — that is, intra macroblocks whose
/// type code does NOT take the [`crate::svq3_mb::IFrameMbType::LumaDcSeparate`]
/// (code 0) / [`crate::svq3_mb::IFrameMbType::LumaDcSeparateNoOthers`]
/// (code 25) branch.
///
/// Returns the intermediate DC value before the trailing `+
/// DEQUANT_ROUND >> DEQUANT_SHIFT` finalisation; chain with
/// [`finalise_dc`] to recover the spec's full
/// `out = (coeff * DEQUANT_COEFF_TABLE[Q] + dc + 0x80000) >> 20`
/// expression with `coeff = 0` (no per-coefficient AC contribution).
#[inline]
#[must_use]
pub const fn dequantize_intra_luma_dc(block_zero: i32) -> i32 {
    INTRA_LUMA_DC_SCALE * block_zero
}

/// Apply the wiki spec's chroma DC dequantization expression
/// `dc = (DEQUANT_COEFF_TABLE[Q] * (block[0] >> 3)) >> 1` for the
/// quantiser `q` and block sample `block_zero`.
///
/// The caller must ensure `q < DEQUANT_COEFF_TABLE_LEN`; this helper
/// is `const fn` and so cannot validate the index dynamically. The
/// returned `i32` is the pre-finalisation chroma DC value; chain
/// with [`finalise_dc`] to recover the full `(coeff * DEQUANT_COEFF_TABLE[Q]
/// + dc + 0x80000) >> 20` expression for the chroma block.
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`. (`const fn` array index
/// out-of-bounds is a compile-time error for static `q`, a runtime
/// panic otherwise.)
#[inline]
#[must_use]
pub const fn dequantize_chroma_dc(q: u32, block_zero: i32) -> i32 {
    let coeff = DEQUANT_COEFF_TABLE[q as usize] as i32;
    (coeff * (block_zero >> CHROMA_DC_PRE_SHIFT)) >> CHROMA_DC_POST_SHIFT
}

/// Apply the wiki spec's general per-coefficient dequantization
/// expression
/// `out = (coeff * DEQUANT_COEFF_TABLE[Q] + dc + 0x80000) >> 20`
/// for the quantiser `q`, residual coefficient `coeff`, and
/// pre-computed `dc` contribution (zero when the block has a
/// separate DC stream and the spec's "if not defined otherwise"
/// branch applies).
///
/// The `coeff` argument is the residual produced by the
/// [`crate::svq3_coeff`] walker after dezigzag; `dc` is the
/// pre-finalisation DC value from [`dequantize_intra_luma_dc`] or
/// [`dequantize_chroma_dc`] (or `0` when no separate DC term
/// applies).
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
#[inline]
#[must_use]
pub const fn dequantize_coefficient(q: u32, coeff: i32, dc: i32) -> i32 {
    let q_scale = DEQUANT_COEFF_TABLE[q as usize] as i32;
    (coeff * q_scale + dc + DEQUANT_ROUND) >> DEQUANT_SHIFT
}

/// Apply the standard rounding finalisation `(x + DEQUANT_ROUND) >>
/// DEQUANT_SHIFT` to a pre-finalisation DC contribution from
/// [`dequantize_intra_luma_dc`] / [`dequantize_chroma_dc`].
///
/// Useful when the caller wants the DC value alone (no AC contribution)
/// — equivalent to [`dequantize_coefficient`] with `coeff = 0`.
#[inline]
#[must_use]
pub const fn finalise_dc(dc: i32) -> i32 {
    (dc + DEQUANT_ROUND) >> DEQUANT_SHIFT
}

/// Apply one 1-D row of the 2×2 chroma DC transform matrix to a 2-point
/// column of samples.
///
/// The wiki spec's §"Macroblock transform and dequantization" pins the
/// 2×2 chroma DC transform matrix as
///
/// ```text
///   8  8
///   8 -8
/// ```
///
/// (also exposed verbatim as [`CHROMA_DC_TRANSFORM_MATRIX`]). The spec
/// states "chroma DCs need to be transformed first using the following
/// matrix" before the [`dequantize_chroma_dc`] expression is applied.
///
/// This helper carries out **one row's** dot product against a 2-point
/// column `[a, b]`, using `matrix_row` as the row of weights. For the
/// first matrix row `[8, 8]` the result is `8 * (a + b)`; for the second
/// matrix row `[8, -8]` the result is `8 * (a - b)`. The two matrix rows
/// are accessible as `CHROMA_DC_TRANSFORM_MATRIX[0]` and
/// `CHROMA_DC_TRANSFORM_MATRIX[1]`.
///
/// Returns the unrounded i32 dot product; subsequent dequantisation /
/// finalisation is the caller's responsibility (see
/// [`dequantize_chroma_dc`] / [`finalise_dc`]).
///
/// # Examples
///
/// Apply both rows of the matrix to the same input pair:
///
/// ```
/// use oxideav_svq::svq3_dequant::{
///     apply_chroma_dc_transform_row, CHROMA_DC_TRANSFORM_MATRIX,
/// };
/// let pair = (3, 1);
/// // Row 0 = sum-of-pair × 8.
/// assert_eq!(
///     apply_chroma_dc_transform_row(CHROMA_DC_TRANSFORM_MATRIX[0], pair.0, pair.1),
///     32,
/// );
/// // Row 1 = difference-of-pair × 8.
/// assert_eq!(
///     apply_chroma_dc_transform_row(CHROMA_DC_TRANSFORM_MATRIX[1], pair.0, pair.1),
///     16,
/// );
/// ```
#[inline]
#[must_use]
pub const fn apply_chroma_dc_transform_row(matrix_row: [i32; 2], a: i32, b: i32) -> i32 {
    matrix_row[0] * a + matrix_row[1] * b
}

/// Apply the 2×2 chroma DC transform matrix to a row-major 2×2 input
/// block by multiplying the matrix into the block's columns (`M · X`).
///
/// The wiki spec's §"Macroblock transform and dequantization" pins the
/// transform matrix [`CHROMA_DC_TRANSFORM_MATRIX`] = `[[8, 8], [8, -8]]`
/// but does not enumerate the full `M · X · M^T` two-sided transform
/// expression; only `M` itself is quoted alongside the remark "chroma
/// DCs need to be transformed first using the following matrix". This
/// helper applies `M` against the input's columns and returns the result
/// in row-major order — the single-sided transform pass that is
/// unambiguously what `M` alone produces against a column vector.
///
/// The input `block` is laid out row-major: `block[0]` = `(0, 0)`,
/// `block[1]` = `(0, 1)`, `block[2]` = `(1, 0)`, `block[3]` = `(1, 1)`.
/// This matches [`crate::svq3_scan::place_chroma_dc_2x2`]'s output. The
/// returned `[i32; 4]` is laid out the same way.
///
/// Per-position output (where `(r, c)` = row, column):
///
/// * `out[0, 0] = M[0, :] · X[:, 0] = 8 * (block[0, 0] + block[1, 0])`
/// * `out[0, 1] = M[0, :] · X[:, 1] = 8 * (block[0, 1] + block[1, 1])`
/// * `out[1, 0] = M[1, :] · X[:, 0] = 8 * (block[0, 0] - block[1, 0])`
/// * `out[1, 1] = M[1, :] · X[:, 1] = 8 * (block[0, 1] - block[1, 1])`
///
/// The unrounded i32 outputs feed directly into [`dequantize_chroma_dc`]
/// for the per-sample dequant step; this helper does NOT apply any
/// shift, bias, or quantiser scaling.
///
/// The full two-sided `M · X · M^T` transform (which the wiki spec does
/// NOT spell out explicitly) is deliberately NOT folded in here — that
/// derivation belongs in a future round once the docs pin it.
///
/// # Examples
///
/// Apply the transform to an identity-like input:
///
/// ```
/// use oxideav_svq::svq3_dequant::apply_chroma_dc_2x2_columns;
/// let block = [1, 0, 0, 1];
/// // out[0,0] = 8 * (1 + 0) = 8;  out[0,1] = 8 * (0 + 1) = 8.
/// // out[1,0] = 8 * (1 - 0) = 8;  out[1,1] = 8 * (0 - 1) = -8.
/// assert_eq!(apply_chroma_dc_2x2_columns(block), [8, 8, 8, -8]);
/// ```
#[inline]
#[must_use]
pub const fn apply_chroma_dc_2x2_columns(block: [i32; 4]) -> [i32; 4] {
    // block layout: row-major 2×2.
    //   block[0] = (0, 0)   block[1] = (0, 1)
    //   block[2] = (1, 0)   block[3] = (1, 1)
    let row0 = CHROMA_DC_TRANSFORM_MATRIX[0];
    let row1 = CHROMA_DC_TRANSFORM_MATRIX[1];
    // Output rows, column by column.
    let out_00 = apply_chroma_dc_transform_row(row0, block[0], block[2]);
    let out_01 = apply_chroma_dc_transform_row(row0, block[1], block[3]);
    let out_10 = apply_chroma_dc_transform_row(row1, block[0], block[2]);
    let out_11 = apply_chroma_dc_transform_row(row1, block[1], block[3]);
    [out_00, out_01, out_10, out_11]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luma_transform_matrix_first_column_all_thirteen() {
        for row in LUMA_TRANSFORM_MATRIX.iter() {
            assert_eq!(row[0], LUMA_TRANSFORM_DC_COLUMN);
        }
    }

    #[test]
    fn luma_transform_matrix_shape() {
        // Verify the matrix is exactly 4×4.
        assert_eq!(LUMA_TRANSFORM_MATRIX.len(), 4);
        for row in LUMA_TRANSFORM_MATRIX.iter() {
            assert_eq!(row.len(), 4);
        }
    }

    #[test]
    fn luma_transform_matrix_verbatim_rows() {
        // Row-by-row verbatim corroboration against the wiki spec's
        // four-row enumeration:
        //   13  17   1   7
        //   13   7  -1 -17
        //   13  -7  -1  17
        //   13 -17   1  -7
        assert_eq!(LUMA_TRANSFORM_MATRIX[0], [13, 17, 1, 7]);
        assert_eq!(LUMA_TRANSFORM_MATRIX[1], [13, 7, -1, -17]);
        assert_eq!(LUMA_TRANSFORM_MATRIX[2], [13, -7, -1, 17]);
        assert_eq!(LUMA_TRANSFORM_MATRIX[3], [13, -17, 1, -7]);
    }

    #[test]
    fn luma_transform_matrix_row_sums_are_dc_quad() {
        // Sum of each row equals the column-0 value times 4 minus
        // the symmetric ±17 ±1 ±7 sub-pattern. For row 0 the AC
        // contribution is 17 + 1 + 7 = 25; row 1 is 7 - 1 - 17 = -11;
        // row 2 is -7 - 1 + 17 = 9; row 3 is -17 + 1 - 7 = -23. All
        // four sum to (13 + 13 + 13 + 13) + (25 - 11 + 9 - 23) = 52
        // + 0 = 52. (Verifies the AC contributions cancel column-
        // wise; matches the "DC-only column 0" remark above.)
        let mut total = 0i32;
        for row in LUMA_TRANSFORM_MATRIX.iter() {
            total += row.iter().sum::<i32>();
        }
        assert_eq!(total, 4 * LUMA_TRANSFORM_DC_COLUMN);
    }

    #[test]
    fn chroma_dc_transform_matrix_verbatim() {
        // Verbatim corroboration against the wiki spec's "8 8 / 8 -8"
        // chroma DC transform matrix.
        assert_eq!(CHROMA_DC_TRANSFORM_MATRIX, [[8, 8], [8, -8]]);
    }

    #[test]
    fn chroma_dc_transform_matrix_row_zero_sums_sixteen() {
        // The first row sums to 16 (the chroma DC's "16 *
        // average-of-pair" component).
        assert_eq!(CHROMA_DC_TRANSFORM_MATRIX[0].iter().sum::<i32>(), 16);
    }

    #[test]
    fn chroma_dc_transform_matrix_row_one_sums_zero() {
        // The second row sums to zero (the chroma DC's
        // "difference-of-pair" component cancels out at row level).
        assert_eq!(CHROMA_DC_TRANSFORM_MATRIX[1].iter().sum::<i32>(), 0);
    }

    #[test]
    fn dequant_table_length_matches_quantiser_range() {
        assert_eq!(DEQUANT_COEFF_TABLE.len(), DEQUANT_COEFF_TABLE_LEN);
        assert_eq!(DEQUANT_COEFF_TABLE_LEN, 32);
        assert_eq!(DEQUANT_QUANTISER_RANGE.end, DEQUANT_COEFF_TABLE_LEN as u32);
        assert_eq!(DEQUANT_QUANTISER_RANGE.start, 0);
    }

    #[test]
    fn dequant_table_first_entry_is_3881() {
        // Verbatim against the wiki spec's first row.
        assert_eq!(DEQUANT_COEFF_TABLE[0], 3881);
    }

    #[test]
    fn dequant_table_last_entry_is_141533() {
        // Verbatim against the wiki spec's last row.
        assert_eq!(DEQUANT_COEFF_TABLE[31], 141533);
    }

    #[test]
    fn dequant_table_row_zero_verbatim() {
        // Verbatim corroboration against wiki spec row 1.
        assert_eq!(
            &DEQUANT_COEFF_TABLE[0..8],
            &[3881, 4351, 4890, 5481, 6154, 6914, 7761, 8718]
        );
    }

    #[test]
    fn dequant_table_row_one_verbatim() {
        // Verbatim corroboration against wiki spec row 2.
        assert_eq!(
            &DEQUANT_COEFF_TABLE[8..16],
            &[9781, 10987, 12339, 13828, 15523, 17435, 19561, 21873]
        );
    }

    #[test]
    fn dequant_table_row_two_verbatim() {
        // Verbatim corroboration against wiki spec row 3.
        assert_eq!(
            &DEQUANT_COEFF_TABLE[16..24],
            &[24552, 27656, 30847, 34870, 38807, 43747, 49103, 54683]
        );
    }

    #[test]
    fn dequant_table_row_three_verbatim() {
        // Verbatim corroboration against wiki spec row 4.
        assert_eq!(
            &DEQUANT_COEFF_TABLE[24..32],
            &[61694, 68745, 77615, 89113, 100253, 109366, 126635, 141533]
        );
    }

    #[test]
    fn dequant_table_is_strictly_monotonic_increasing() {
        for i in 1..DEQUANT_COEFF_TABLE_LEN {
            assert!(
                DEQUANT_COEFF_TABLE[i] > DEQUANT_COEFF_TABLE[i - 1],
                "table not strictly increasing at index {}: {} <= {}",
                i,
                DEQUANT_COEFF_TABLE[i],
                DEQUANT_COEFF_TABLE[i - 1]
            );
        }
    }

    #[test]
    fn dequant_shift_is_twenty() {
        assert_eq!(DEQUANT_SHIFT, 20);
    }

    #[test]
    fn dequant_round_is_half_of_shift_unit() {
        // Standard `(x + 2^{n-1}) >> n` round-half-up: 2^{20-1} = 2^19 = 0x80000.
        assert_eq!(DEQUANT_ROUND, 1 << (DEQUANT_SHIFT - 1));
        assert_eq!(DEQUANT_ROUND, 0x80000);
        assert_eq!(DEQUANT_ROUND, 524_288);
    }

    #[test]
    fn intra_luma_dc_scale_decomposes_to_thirteen_squared_times_tail() {
        assert_eq!(INTRA_LUMA_DC_SCALE_TAIL, 1538);
        assert_eq!(
            INTRA_LUMA_DC_SCALE,
            LUMA_TRANSFORM_DC_COLUMN * LUMA_TRANSFORM_DC_COLUMN * INTRA_LUMA_DC_SCALE_TAIL
        );
        // Cross-check the numeric value: 13 * 13 = 169; 169 * 1538
        // = 259_922.
        assert_eq!(INTRA_LUMA_DC_SCALE, 13 * 13 * 1538);
        assert_eq!(INTRA_LUMA_DC_SCALE, 259_922);
    }

    #[test]
    fn chroma_dc_shifts_sum_to_four() {
        // The total `>> 4` shift the chroma DC formula imposes
        // matches the chroma transform matrix's `8` scale factor.
        assert_eq!(CHROMA_DC_PRE_SHIFT + CHROMA_DC_POST_SHIFT, 4);
        assert_eq!(CHROMA_DC_PRE_SHIFT, 3);
        assert_eq!(CHROMA_DC_POST_SHIFT, 1);
    }

    #[test]
    fn dequantize_intra_luma_dc_zero_input_is_zero() {
        assert_eq!(dequantize_intra_luma_dc(0), 0);
    }

    #[test]
    fn dequantize_intra_luma_dc_one_input_is_scale_value() {
        // The bare wiki-spec expression `13 * 13 * 1538 * block[0]`
        // with `block[0] = 1` returns the scale value itself
        // (= 259_922).
        assert_eq!(dequantize_intra_luma_dc(1), INTRA_LUMA_DC_SCALE);
        assert_eq!(dequantize_intra_luma_dc(1), 259_922);
    }

    #[test]
    fn dequantize_intra_luma_dc_negative_one_input_is_negative_scale() {
        assert_eq!(dequantize_intra_luma_dc(-1), -INTRA_LUMA_DC_SCALE);
    }

    #[test]
    fn dequantize_intra_luma_dc_two_input_doubles_scale() {
        assert_eq!(dequantize_intra_luma_dc(2), 2 * INTRA_LUMA_DC_SCALE);
    }

    #[test]
    fn dequantize_chroma_dc_zero_input_is_zero() {
        for q in 0..DEQUANT_COEFF_TABLE_LEN as u32 {
            assert_eq!(dequantize_chroma_dc(q, 0), 0);
        }
    }

    #[test]
    fn dequantize_chroma_dc_at_q_zero_block_eight() {
        // For q=0, table[0]=3881, block_zero=8:
        // (3881 * (8 >> 3)) >> 1 = (3881 * 1) >> 1 = 1940.
        assert_eq!(dequantize_chroma_dc(0, 8), 1940);
    }

    #[test]
    fn dequantize_chroma_dc_at_q_zero_block_seven() {
        // For q=0, table[0]=3881, block_zero=7:
        // (3881 * (7 >> 3)) >> 1 = (3881 * 0) >> 1 = 0. The chroma
        // formula's `>> 3` discards the low 3 bits of block_zero, so
        // any sub-eight value vanishes.
        assert_eq!(dequantize_chroma_dc(0, 7), 0);
    }

    #[test]
    fn dequantize_chroma_dc_at_q_thirty_one_block_sixteen() {
        // For q=31, table[31]=141533, block_zero=16:
        // (141533 * (16 >> 3)) >> 1 = (141533 * 2) >> 1 = 141533.
        assert_eq!(dequantize_chroma_dc(31, 16), 141_533);
    }

    #[test]
    fn dequantize_chroma_dc_negative_block_negates_result() {
        // The arithmetic-right-shift of a negative integer rounds
        // toward negative infinity, so `(-8) >> 3 = -1`. Then
        // (3881 * -1) >> 1 = -3881 >> 1 = -1941 (arithmetic shift
        // rounds toward negative infinity for odd negatives).
        assert_eq!(dequantize_chroma_dc(0, -8), -1941);
    }

    #[test]
    fn dequantize_coefficient_zero_coeff_zero_dc_rounds_to_zero() {
        // The bias `+ 0x80000` followed by `>> 20` of zero gives
        // `0x80000 >> 20 = 0`.
        for q in 0..DEQUANT_COEFF_TABLE_LEN as u32 {
            assert_eq!(dequantize_coefficient(q, 0, 0), 0);
        }
    }

    #[test]
    fn dequantize_coefficient_unit_coeff_at_q_zero() {
        // For q=0, coeff=1, dc=0: (1 * 3881 + 0 + 0x80000) >> 20.
        // 3881 + 524288 = 528169. 528169 >> 20 = 0.
        assert_eq!(dequantize_coefficient(0, 1, 0), 0);
    }

    #[test]
    fn dequantize_coefficient_large_coeff_at_q_zero() {
        // For q=0, coeff=512, dc=0: (512 * 3881 + 0x80000) >> 20.
        // 512 * 3881 = 1_987_072. + 524288 = 2_511_360.
        // >> 20 = 2_511_360 / 1_048_576 = 2 (integer division).
        assert_eq!(dequantize_coefficient(0, 512, 0), 2);
    }

    #[test]
    fn dequantize_coefficient_large_coeff_at_q_thirty_one() {
        // For q=31, coeff=8, dc=0: (8 * 141533 + 0x80000) >> 20.
        // 8 * 141533 = 1_132_264. + 524288 = 1_656_552.
        // >> 20 = 1.
        assert_eq!(dequantize_coefficient(31, 8, 0), 1);
    }

    #[test]
    fn dequantize_coefficient_with_dc_contribution() {
        // For q=0, coeff=0, dc=INTRA_LUMA_DC_SCALE (= 259_922):
        // (0 + 259_922 + 0x80000) >> 20 = 784_210 >> 20 = 0.
        // The intra-luma DC formula doesn't "round up" a single
        // block_zero=1 to anything; sample-level reconstruction will
        // need the full sum across the 4×4 block.
        let dc = dequantize_intra_luma_dc(1);
        assert_eq!(dequantize_coefficient(0, 0, dc), 0);
    }

    #[test]
    fn dequantize_coefficient_negative_coeff_rounds_toward_negative_infinity() {
        // Arithmetic right shift on negative numerator: for q=0,
        // coeff=-512, dc=0: (-512 * 3881 + 0x80000) >> 20.
        // -512 * 3881 = -1_987_072. + 524288 = -1_462_784.
        // -1_462_784 >> 20 = -2 (arithmetic shift floors toward -inf).
        assert_eq!(dequantize_coefficient(0, -512, 0), -2);
    }

    #[test]
    fn finalise_dc_zero_is_zero() {
        assert_eq!(finalise_dc(0), 0);
    }

    #[test]
    fn finalise_dc_matches_dequantize_coefficient_with_zero_coeff() {
        // finalise_dc(dc) is by construction
        // dequantize_coefficient(_, 0, dc) for any q (q is unused
        // when coeff is 0).
        for dc in [-1_000_000, -1, 0, 1, 1_000_000, INTRA_LUMA_DC_SCALE] {
            assert_eq!(finalise_dc(dc), dequantize_coefficient(0, 0, dc));
        }
    }

    #[test]
    fn finalise_dc_round_half_up_at_boundary() {
        // dc = 524288 (= DEQUANT_ROUND) plus the round bias = 1_048_576.
        // >> 20 = 1. This is the half-up boundary case.
        assert_eq!(finalise_dc(DEQUANT_ROUND), 1);
        // dc = 524287 (one less) plus the round bias = 1_048_575.
        // >> 20 = 0.
        assert_eq!(finalise_dc(DEQUANT_ROUND - 1), 0);
    }

    #[test]
    fn dequantize_coefficient_for_every_quantiser_with_unit_coeff() {
        // Sanity sweep: for coeff=512 (large enough to be visible
        // after the >> 20), dc=0, the result is non-decreasing in q
        // because DEQUANT_COEFF_TABLE itself is strictly monotonic.
        let mut prev = -1i32;
        for q in 0..DEQUANT_COEFF_TABLE_LEN as u32 {
            let out = dequantize_coefficient(q, 512, 0);
            assert!(
                out >= prev,
                "dequantize_coefficient regressed at q={}: {} < {}",
                q,
                out,
                prev
            );
            prev = out;
        }
    }

    #[test]
    fn dequantize_coefficient_is_linear_in_coeff() {
        // For fixed q + dc=0, doubling coeff doubles the pre-shift
        // numerator and (modulo the round-half-up bias) also doubles
        // the shifted output. Check the exact arithmetic.
        for q in [0u32, 7, 15, 23, 31] {
            let scale = DEQUANT_COEFF_TABLE[q as usize] as i32;
            for &coeff in &[100i32, 250, 500, 1000] {
                let expected = (coeff * scale + DEQUANT_ROUND) >> DEQUANT_SHIFT;
                assert_eq!(dequantize_coefficient(q, coeff, 0), expected);
            }
        }
    }

    #[test]
    fn dequantize_coefficient_is_additive_in_dc() {
        // For fixed q + coeff=0, the output is exactly
        // finalise_dc(dc) for any dc.
        for q in [0u32, 5, 17, 31] {
            for &dc in &[
                -1_000_000i32,
                -INTRA_LUMA_DC_SCALE,
                0,
                INTRA_LUMA_DC_SCALE,
                1_000_000,
            ] {
                assert_eq!(dequantize_coefficient(q, 0, dc), finalise_dc(dc));
            }
        }
    }

    // ---- 2×2 chroma DC transform application ---------------------------

    #[test]
    fn chroma_dc_transform_row_zero_is_sum_times_eight() {
        // Row 0 of the matrix is `[8, 8]`; the dot product against any
        // 2-point column `[a, b]` is `8 * (a + b)`.
        let row0 = CHROMA_DC_TRANSFORM_MATRIX[0];
        for &(a, b) in &[(0i32, 0i32), (1, 0), (0, 1), (3, 5), (-2, 7), (-4, -1)] {
            assert_eq!(
                apply_chroma_dc_transform_row(row0, a, b),
                8 * (a + b),
                "row 0 mismatch for ({a}, {b})"
            );
        }
    }

    #[test]
    fn chroma_dc_transform_row_one_is_difference_times_eight() {
        // Row 1 of the matrix is `[8, -8]`; the dot product against any
        // 2-point column `[a, b]` is `8 * (a - b)`.
        let row1 = CHROMA_DC_TRANSFORM_MATRIX[1];
        for &(a, b) in &[(0i32, 0i32), (1, 0), (0, 1), (3, 5), (-2, 7), (-4, -1)] {
            assert_eq!(
                apply_chroma_dc_transform_row(row1, a, b),
                8 * (a - b),
                "row 1 mismatch for ({a}, {b})"
            );
        }
    }

    #[test]
    fn chroma_dc_transform_row_zero_at_known_pair() {
        // Worked example: row 0 dot (3, 1) = 8 * 3 + 8 * 1 = 32.
        assert_eq!(
            apply_chroma_dc_transform_row(CHROMA_DC_TRANSFORM_MATRIX[0], 3, 1),
            32
        );
    }

    #[test]
    fn chroma_dc_transform_row_one_at_known_pair() {
        // Worked example: row 1 dot (3, 1) = 8 * 3 + (-8) * 1 = 16.
        assert_eq!(
            apply_chroma_dc_transform_row(CHROMA_DC_TRANSFORM_MATRIX[1], 3, 1),
            16
        );
    }

    #[test]
    fn chroma_dc_2x2_columns_all_zero_block_yields_all_zero_output() {
        // Identity for the additive zero input.
        assert_eq!(apply_chroma_dc_2x2_columns([0, 0, 0, 0]), [0, 0, 0, 0]);
    }

    #[test]
    fn chroma_dc_2x2_columns_top_row_one_zero_block() {
        // Input:
        //   1 0
        //   0 0
        // out[0,0] = 8 * (1 + 0) = 8.   out[0,1] = 8 * (0 + 0) = 0.
        // out[1,0] = 8 * (1 - 0) = 8.   out[1,1] = 8 * (0 - 0) = 0.
        assert_eq!(apply_chroma_dc_2x2_columns([1, 0, 0, 0]), [8, 0, 8, 0]);
    }

    #[test]
    fn chroma_dc_2x2_columns_bottom_row_one_zero_block() {
        // Input:
        //   0 0
        //   1 0
        // out[0,0] = 8 * (0 + 1) = 8.   out[0,1] = 8 * (0 + 0) = 0.
        // out[1,0] = 8 * (0 - 1) = -8.  out[1,1] = 8 * (0 - 0) = 0.
        assert_eq!(apply_chroma_dc_2x2_columns([0, 0, 1, 0]), [8, 0, -8, 0]);
    }

    #[test]
    fn chroma_dc_2x2_columns_diagonal_one_zero_block() {
        // Input:
        //   1 0
        //   0 1
        // out[0,0] = 8 * (1 + 0) = 8.   out[0,1] = 8 * (0 + 1) = 8.
        // out[1,0] = 8 * (1 - 0) = 8.   out[1,1] = 8 * (0 - 1) = -8.
        assert_eq!(apply_chroma_dc_2x2_columns([1, 0, 0, 1]), [8, 8, 8, -8]);
    }

    #[test]
    fn chroma_dc_2x2_columns_anti_diagonal_one_zero_block() {
        // Input:
        //   0 1
        //   1 0
        // out[0,0] = 8 * (0 + 1) = 8.   out[0,1] = 8 * (1 + 0) = 8.
        // out[1,0] = 8 * (0 - 1) = -8.  out[1,1] = 8 * (1 - 0) = 8.
        assert_eq!(apply_chroma_dc_2x2_columns([0, 1, 1, 0]), [8, 8, -8, 8]);
    }

    #[test]
    fn chroma_dc_2x2_columns_all_ones_block_doubles_dc_and_cancels_diff() {
        // Input:
        //   1 1
        //   1 1
        // out[0,0] = 8 * (1 + 1) = 16.  out[0,1] = 8 * (1 + 1) = 16.
        // out[1,0] = 8 * (1 - 1) = 0.   out[1,1] = 8 * (1 - 1) = 0.
        assert_eq!(apply_chroma_dc_2x2_columns([1, 1, 1, 1]), [16, 16, 0, 0]);
    }

    #[test]
    fn chroma_dc_2x2_columns_is_linear_in_input() {
        // Doubling every input position doubles every output position.
        let block = [3, -2, 5, 1];
        let doubled = [6, -4, 10, 2];
        let out = apply_chroma_dc_2x2_columns(block);
        let out_doubled = apply_chroma_dc_2x2_columns(doubled);
        for (o, od) in out.iter().zip(out_doubled.iter()) {
            assert_eq!(2 * o, *od);
        }
    }

    #[test]
    fn chroma_dc_2x2_columns_negation_negates_output() {
        // f(-X) = -f(X) — the transform is linear.
        let block = [3, -2, 5, 1];
        let negated = [-3, 2, -5, -1];
        let out = apply_chroma_dc_2x2_columns(block);
        let out_negated = apply_chroma_dc_2x2_columns(negated);
        for (o, on) in out.iter().zip(out_negated.iter()) {
            assert_eq!(-*o, *on);
        }
    }

    #[test]
    fn chroma_dc_2x2_columns_top_row_only_carries_to_both_output_rows() {
        // Only top-row input → both output rows are non-zero with the
        // same magnitudes (sum and difference of the top-row pair both
        // collapse to the top-row pair when bottom is zero).
        let block = [2, 5, 0, 0];
        // out[0,0] = 8 * (2 + 0) = 16. out[0,1] = 8 * (5 + 0) = 40.
        // out[1,0] = 8 * (2 - 0) = 16. out[1,1] = 8 * (5 - 0) = 40.
        assert_eq!(apply_chroma_dc_2x2_columns(block), [16, 40, 16, 40]);
    }

    #[test]
    fn chroma_dc_2x2_columns_bottom_row_only_signs_second_output_row() {
        // Only bottom-row input → first output row carries the bottom
        // values verbatim (sum) and second output row carries their
        // negations (difference 0 - x = -x).
        let block = [0, 0, 3, -1];
        // out[0,0] = 8 * (0 + 3) = 24.  out[0,1] = 8 * (0 - 1) = -8.
        // out[1,0] = 8 * (0 - 3) = -24. out[1,1] = 8 * (0 - -1) = 8.
        assert_eq!(apply_chroma_dc_2x2_columns(block), [24, -8, -24, 8]);
    }

    #[test]
    fn chroma_dc_2x2_columns_output_row_zero_is_column_wise_sum_times_eight() {
        // Output row 0 column c = 8 * (block top-row c + block bottom-row c).
        let block = [7, -3, 2, 4];
        let out = apply_chroma_dc_2x2_columns(block);
        assert_eq!(out[0], 8 * (block[0] + block[2]));
        assert_eq!(out[1], 8 * (block[1] + block[3]));
    }

    #[test]
    fn chroma_dc_2x2_columns_output_row_one_is_column_wise_difference_times_eight() {
        // Output row 1 column c = 8 * (block top-row c - block bottom-row c).
        let block = [7, -3, 2, 4];
        let out = apply_chroma_dc_2x2_columns(block);
        assert_eq!(out[2], 8 * (block[0] - block[2]));
        assert_eq!(out[3], 8 * (block[1] - block[3]));
    }

    #[test]
    fn chroma_dc_2x2_columns_const_evaluable_in_static_context() {
        // The `const fn` annotation lets the helper be used at const
        // evaluation time.
        const OUT: [i32; 4] = apply_chroma_dc_2x2_columns([1, 2, 3, 4]);
        // out[0,0] = 8*(1+3)=32. out[0,1] = 8*(2+4)=48.
        // out[1,0] = 8*(1-3)=-16. out[1,1] = 8*(2-4)=-16.
        assert_eq!(OUT, [32, 48, -16, -16]);
    }

    #[test]
    fn chroma_dc_2x2_columns_chains_with_place_chroma_dc_2x2() {
        // Cross-module sanity: feed a placement output through the
        // transform to confirm the row-major flat layout is compatible.
        // A single coefficient at scan position 0 with value 1 places at
        // flat index 0 of the 4-entry block — the (0, 0) position. The
        // transformed output's column 0 sums (top + bottom) and
        // differences (top - bottom) collapse to (1, 1) and (1, -1)
        // times 8 = (8, 8). Column 1 is all zeros.
        use crate::svq3_coeff::Coefficient;
        use crate::svq3_scan::place_chroma_dc_2x2;
        let block = place_chroma_dc_2x2(&[Coefficient { run: 0, value: 1 }]).unwrap();
        assert_eq!(block, [1, 0, 0, 0]);
        let transformed = apply_chroma_dc_2x2_columns(block);
        assert_eq!(transformed, [8, 0, 8, 0]);
    }
}
