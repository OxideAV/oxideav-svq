//! SVQ3 macroblock transform and dequantization arithmetic
//! (structural).
//!
//! ## Provenance
//!
//! This module carries the transform / dequantisation arithmetic of
//! `docs/video/svq3/spec/04-dc-secondary-transform.md` and
//! `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 2:
//!
//! * The 4×4 core inverse transform basis (spec/04 §1, **measured**):
//!
//!   ```text
//!     13  17  13   7
//!     13   7 -13 -17
//!     13  -7 -13  17
//!     13 -17  13  -7
//!   ```
//!
//!   The in-repo wiki snapshot prints the third column as `1, −1,
//!   −1, 1`; spec/04 §1 corrects it to `13, −13, −13, 13` — all four
//!   basis vectors then share the squared norm `676`, and the measured
//!   input→output relations (a `2²⁰` coefficient at position 2 yields
//!   `±169`, not `±13`) pin the corrected column.
//!
//! * A 32-entry quantiser ladder indexed by the quantiser `Q ∈ 0..32`
//!   — exposed as [`DEQUANT_COEFF_TABLE`] — and the 32-entry **chroma
//!   quantiser remap** of spec/04 §3 ([`CHROMA_QUANTISER_INDEX`]):
//!   chroma blocks (DC and AC) index the ladder through the remap,
//!   luma blocks index it directly.
//!
//! * The 2×2 **chroma DC secondary transform** of spec/04 §2: the four
//!   dequantised chroma DC values pass through the unnormalised 2×2
//!   Hadamard and are halved with **truncation toward zero**
//!   ([`chroma_dc_secondary_transform`]), then scatter to coefficient
//!   position 0 of the four 4×4 chroma blocks in raster order.
//!
//! * The **luma DC secondary transform** of spec/04 §4
//!   ([`luma_dc_secondary_transform`]): the separate luma DC block of
//!   an intra 16×16 macroblock is dequantised with the **luma**
//!   quantiser, run through the ordinary core transform (fused
//!   `+0x80000 >> 20`), and each of the sixteen results is multiplied
//!   by the literal `1538`; result *k* becomes coefficient 0 of luma
//!   block *k* in raster order.
//!
//! * The dequantisation closed forms:
//!     * intra luma without separate DC block:
//!       `dc = 13 * 13 * 1538 * block[0]` (additive, post-transform)
//!     * general per-coefficient dequant (Gap 2):
//!       `out = (coeff * DEQUANT_COEFF_TABLE[Q] + dc + 0x80000) >> 20`
//!       where `dc = 0` if not defined otherwise.
//!
//! ## Residual interleave (spec/01 Gap 2)
//!
//! The transform is the **two-sided** `M · X · Mᵀ` (a rows pass then a
//! columns pass over the same kernel), and the dequantisation is
//! **fused into the same pass** as the inverse transform — the
//! per-element store is
//! `out = (coeff·DEQUANT_COEFF_TABLE[Q] + dc + 0x80000) >> 20`, with
//! the single `>> 20` ([`DEQUANT_SHIFT`]) being the *only*
//! post-transform shift. The luma residual-interleave pipeline is
//! [`dequantize_transform_luma_block`] (and its additive-DC variant
//! [`dequantize_transform_luma_block_with_dc`]): place →
//! per-coefficient dequant-scale → two-sided transform → fused
//! `+ dc + 0x80000 >> 20`. The general dequantization shift of `20`
//! combined with the `+ 0x80000` (`= 1 << 19`) additive bias is the
//! standard `(x + 2^{n-1}) >> n` round-half-up step.
//!
//! An additive post-transform `dc` of `169 · v` is arithmetically
//! identical to placing `v` at coefficient position 0 before the
//! transform (column 0 of the basis is uniformly 13, so a position-0
//! coefficient contributes `13 · 13 · v` to every pre-shift element);
//! the reconstruction layer uses the additive form for both secondary
//! transforms’ scattered DC terms.

use core::ops::Range;

/// The 4×4 core inverse transform basis, per
/// `docs/video/svq3/spec/04-dc-secondary-transform.md` §1 (measured):
///
/// ```text
///   13  17  13   7
///   13   7 -13 -17
///   13  -7 -13  17
///   13 -17  13  -7
/// ```
///
/// The wiki snapshot prints the third column as `1, −1, −1, 1`;
/// spec/04 §1 corrects it to `13, −13, −13, 13` (measured: a `2²⁰`
/// coefficient at position 2 produces `±169 = ±13·13`, and only the
/// corrected column gives all four basis vectors the shared squared
/// norm `4·13² = 17² + 2·7² + 17² = 676`).
///
/// Indexed `[row][col]` so `LUMA_TRANSFORM_MATRIX[0][0] = 13`,
/// `LUMA_TRANSFORM_MATRIX[3][3] = -7`. The four rows share the
/// constant column-0 value `13` — see [`LUMA_TRANSFORM_DC_COLUMN`].
pub const LUMA_TRANSFORM_MATRIX: [[i32; 4]; 4] = [
    [13, 17, 13, 7],
    [13, 7, -13, -17],
    [13, -7, -13, 17],
    [13, -17, 13, -7],
];

/// The column-0 value of [`LUMA_TRANSFORM_MATRIX`].
///
/// All four rows of the luma transform matrix have `13` in their
/// first column. This constant is exposed for compile-time
/// corroboration of that invariant; the intra-luma-DC scale
/// [`INTRA_LUMA_DC_SCALE`] folds it in as `13 * 13 * 1538`.
pub const LUMA_TRANSFORM_DC_COLUMN: i32 = 13;

/// The 32-entry chroma quantiser remap of
/// `docs/video/svq3/spec/04-dc-secondary-transform.md` §3
/// (`docs/video/svq3/tables/02-chroma-quantiser-index.csv`).
///
/// Luma coefficient blocks index [`DEQUANT_COEFF_TABLE`] with the
/// macroblock quantiser directly; **chroma blocks — both the 2×2 DC
/// block and the AC coefficients — index it with this remapped
/// value**: the identity for quantisers 0…17, then a compression that
/// saturates at ladder entry 25 (`68745`).
pub const CHROMA_QUANTISER_INDEX: [u32; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, // identity 0..=17
    17, 18, 19, 20, 20, 21, 22, 22, 23, 23, 24, 24, 25, 25,
];

/// Remap a macroblock quantiser to the chroma quantiser index
/// (spec/04 §3): `chroma_index = CHROMA_QUANTISER_INDEX[q]`.
///
/// The returned index is what chroma blocks (DC and AC) use to index
/// [`DEQUANT_COEFF_TABLE`]; pass it wherever a dequant helper takes a
/// quantiser argument for a chroma block.
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
#[inline]
#[must_use]
pub const fn chroma_quantiser_index(q: u32) -> u32 {
    CHROMA_QUANTISER_INDEX[q as usize]
}

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

/// Saturate a 64-bit intermediate back into the `i32` value domain.
///
/// The dequantization / transform helpers in this module compute
/// their products and sums in 64-bit and saturate the result to
/// `i32` on return. Every coefficient magnitude a conforming stream
/// carries keeps the whole pipeline far inside the `i32` domain, so
/// in-domain results are bit-identical to a plain 32-bit evaluation
/// of the spec formulas; the widening only guarantees that HOSTILE
/// coefficient magnitudes (the escape constructions in
/// [`crate::svq3_coeff`] admit levels up to ≈ 2^27 from untrusted
/// bits) cannot overflow — the residual is bounded by the `Clip1`
/// writeback downstream regardless.
#[inline]
#[must_use]
const fn sat_i32(v: i64) -> i32 {
    if v > i32::MAX as i64 {
        i32::MAX
    } else if v < i32::MIN as i64 {
        i32::MIN
    } else {
        v as i32
    }
}

/// Apply the wiki spec's intra-luma DC expression `dc = 13 * 13 *
/// 1538 * block[0]` to the single argument `block_zero`.
///
/// The wiki spec uses this expression "for intra luma blocks without
/// separate DC coefficients block" — that is, intra macroblocks whose
/// type code does NOT take the [`crate::svq3_mb::IFrameMbType::LumaDcSeparate`]
/// (code 0) / [`crate::svq3_mb::IFrameMbType::LumaDcSeparateNoOthers`]
/// (code 25) branch.
///
/// Returns the intermediate (64-bit) DC value before the trailing
/// `+ DEQUANT_ROUND >> DEQUANT_SHIFT` finalisation; feed it to
/// [`dequantize_transform_luma_block_with_dc`] as the additive `dc`
/// term of the fused store.
#[inline]
#[must_use]
pub const fn dequantize_intra_luma_dc(block_zero: i32) -> i64 {
    INTRA_LUMA_DC_SCALE as i64 * block_zero as i64
}

/// The 2×2 chroma DC secondary transform of
/// `docs/video/svq3/spec/04-dc-secondary-transform.md` §2.2, applied
/// to the four **already-dequantised** chroma DC values `c0 c1 c2 c3`
/// in coded order (the 2×2 raster `[[c0, c1], [c2, c3]]`):
///
/// ```text
///   B0 = (c0 + c1 + c2 + c3) / 2
///   B1 = (c0 - c1 + c2 - c3) / 2
///   B2 = (c0 + c1 - c2 - c3) / 2
///   B3 = (c0 - c1 - c2 + c3) / 2
/// ```
///
/// — the unnormalised 2×2 Hadamard `H·X·Hᵀ` followed by a division by
/// two that **truncates toward zero** (spec/04 §2.2: input
/// `(−3, 0, 0, 0)` produces `(−1, −1, −1, −1)`, not the `−2`s an
/// arithmetic shift would give). `B_k` becomes coefficient position 0
/// of chroma 4×4 block *k* in raster order (§2.3), which in the fused
/// per-element store is the additive term `169 · B_k`.
///
/// Sums are computed in 64-bit and saturated back to `i32`, keeping
/// hostile magnitudes safe while leaving every conforming value
/// bit-identical.
#[inline]
#[must_use]
pub const fn chroma_dc_secondary_transform(c: [i32; 4]) -> [i32; 4] {
    let (c0, c1, c2, c3) = (c[0] as i64, c[1] as i64, c[2] as i64, c[3] as i64);
    [
        sat_i32((c0 + c1 + c2 + c3) / 2),
        sat_i32((c0 - c1 + c2 - c3) / 2),
        sat_i32((c0 + c1 - c2 - c3) / 2),
        sat_i32((c0 - c1 - c2 + c3) / 2),
    ]
}

/// Run spec/04 §2.1 steps 2–3 on the four decoded chroma DC levels:
/// dequantise each level with the **chroma** quantiser index
/// (`dc_j = level_j × DEQUANT_COEFF_TABLE[chroma_quantiser_index(q)]`,
/// no rounding term — the value stays at the 2²⁰ scale), then apply
/// the [`chroma_dc_secondary_transform`] butterfly.
///
/// `q` is the **macroblock quantiser** — the remap is applied
/// internally. The returned `[B0, B1, B2, B3]` are the raster-order
/// per-block DC terms of §2.3 (each destined for coefficient position
/// 0 of its 4×4 chroma block, i.e. the additive term `169 · B_k` in
/// the fused store).
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
#[inline]
#[must_use]
pub const fn dequantize_chroma_dc_levels(q: u32, levels: [i32; 4]) -> [i32; 4] {
    let scale = DEQUANT_COEFF_TABLE[chroma_quantiser_index(q) as usize] as i64;
    let deq = [
        sat_i32(levels[0] as i64 * scale),
        sat_i32(levels[1] as i64 * scale),
        sat_i32(levels[2] as i64 * scale),
        sat_i32(levels[3] as i64 * scale),
    ];
    chroma_dc_secondary_transform(deq)
}

/// Apply the wiki spec's general per-coefficient dequantization
/// expression
/// `out = (coeff * DEQUANT_COEFF_TABLE[Q] + dc + 0x80000) >> 20`
/// for the quantiser `q`, residual coefficient `coeff`, and
/// pre-computed `dc` contribution (zero when the block has a
/// separate DC stream and the spec's "if not defined otherwise"
/// branch applies).
///
/// The `coeff` argument is a placed residual level from the
/// [`crate::svq3_coeff`] block decoders; `dc` is a pre-finalisation
/// additive DC term (or `0` when no separate DC term applies).
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
#[inline]
#[must_use]
pub const fn dequantize_coefficient(q: u32, coeff: i32, dc: i32) -> i32 {
    let q_scale = DEQUANT_COEFF_TABLE[q as usize] as i64;
    sat_i32((coeff as i64 * q_scale + dc as i64 + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT)
}

/// Apply the standard rounding finalisation `(x + DEQUANT_ROUND) >>
/// DEQUANT_SHIFT` to a pre-finalisation DC contribution.
///
/// Useful when the caller wants the DC value alone (no AC contribution)
/// — equivalent to [`dequantize_coefficient`] with `coeff = 0`.
#[inline]
#[must_use]
pub const fn finalise_dc(dc: i32) -> i32 {
    sat_i32((dc as i64 + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT)
}

/// Apply one 1-D row of the 4×4 luma transform matrix to a 4-point
/// column of samples.
///
/// `docs/video/svq3/spec/04-dc-secondary-transform.md` §1 pins the 4×4
/// core inverse transform basis (measured) as
///
/// ```text
///   13  17  13   7
///   13   7 -13 -17
///   13  -7 -13  17
///   13 -17  13  -7
/// ```
///
/// (exposed as [`LUMA_TRANSFORM_MATRIX`], including the spec/04 §1
/// third-column correction over the wiki snapshot).
///
/// This helper carries out **one row's** dot product against a 4-point
/// column `[a, b, c, d]`, using `matrix_row` as the row of weights:
/// `matrix_row[0] * a + matrix_row[1] * b + matrix_row[2] * c +
/// matrix_row[3] * d`. The four matrix rows are accessible as
/// `LUMA_TRANSFORM_MATRIX[0]` through `LUMA_TRANSFORM_MATRIX[3]`. Because
/// every row shares the column-0 value [`LUMA_TRANSFORM_DC_COLUMN`] = `13`,
/// the `a` term always contributes `13 * a`.
///
/// Returns the unrounded i32 dot product; subsequent dequantisation /
/// finalisation is the caller's responsibility (see
/// [`dequantize_coefficient`] / [`finalise_dc`]).
///
/// # Examples
///
/// Apply the first matrix row to a column of samples:
///
/// ```
/// use oxideav_svq::svq3_dequant::{
///     apply_luma_transform_row, LUMA_TRANSFORM_MATRIX,
/// };
/// // Row 0 = [13, 17, 13, 7] applied to [1, 1, 1, 1] sums the weights.
/// assert_eq!(
///     apply_luma_transform_row(LUMA_TRANSFORM_MATRIX[0], 1, 1, 1, 1),
///     13 + 17 + 13 + 7,
/// );
/// // A pure-DC column [a, 0, 0, 0] yields 13 * a for every row.
/// assert_eq!(
///     apply_luma_transform_row(LUMA_TRANSFORM_MATRIX[3], 5, 0, 0, 0),
///     13 * 5,
/// );
/// ```
#[inline]
#[must_use]
pub const fn apply_luma_transform_row(matrix_row: [i32; 4], a: i32, b: i32, c: i32, d: i32) -> i32 {
    sat_i32(
        matrix_row[0] as i64 * a as i64
            + matrix_row[1] as i64 * b as i64
            + matrix_row[2] as i64 * c as i64
            + matrix_row[3] as i64 * d as i64,
    )
}

/// Apply the 4×4 luma transform matrix to a row-major 4×4 input block by
/// multiplying the matrix into the block's columns (`M · X`).
///
/// The wiki spec's §"Macroblock transform and dequantization" pins the
/// transform matrix [`LUMA_TRANSFORM_MATRIX`] but does not enumerate the
/// full `M · X · M^T` two-sided transform expression; only `M` itself is
/// quoted under "Transform coefficients". This helper applies `M` against
/// the input's columns and returns the result in row-major order — the
/// single-sided `M · X` pass of the two-sided transform.
///
/// The input `block` is laid out row-major: `block[r * 4 + c]` is the
/// sample at row `r`, column `c` (`r, c ∈ 0..4`). The returned `[i32; 16]`
/// is laid out the same way: `out[r * 4 + c] = M[r, :] · X[:, c]`.
///
/// The unrounded i32 outputs feed into [`dequantize_coefficient`] for the
/// per-sample dequant step; this helper does NOT apply any shift, bias, or
/// quantiser scaling.
///
/// The full two-sided `M · X · M^T` transform (which the wiki spec does
/// NOT spell out explicitly) is deliberately NOT folded in here — that
/// derivation belongs in a future round once the docs pin it.
///
/// # Examples
///
/// A pure-DC column input (column 0 active, others zero) reproduces the
/// shared column-0 weight `13` for every output row:
///
/// ```
/// use oxideav_svq::svq3_dequant::apply_luma_transform_columns;
/// // Column 0 holds [2, 0, 0, 0]^T (block[0] = 2, rest of column 0 = 0).
/// let mut block = [0i32; 16];
/// block[0] = 2; // (row 0, col 0)
/// let out = apply_luma_transform_columns(block);
/// // out[r][0] = 13 * 2 for every row r.
/// assert_eq!(out[0], 26);
/// assert_eq!(out[4], 26);
/// assert_eq!(out[8], 26);
/// assert_eq!(out[12], 26);
/// ```
#[inline]
#[must_use]
pub const fn apply_luma_transform_columns(block: [i32; 16]) -> [i32; 16] {
    // block layout: row-major 4×4 (block[r * 4 + c] = sample at (r, c)).
    let mut out = [0i32; 16];
    // out[r * 4 + c] = M[r, :] · X[:, c]
    //              = sum over k of M[r][k] * block[k * 4 + c]
    let mut r = 0;
    while r < 4 {
        let row = LUMA_TRANSFORM_MATRIX[r];
        let mut c = 0;
        while c < 4 {
            out[r * 4 + c] =
                apply_luma_transform_row(row, block[c], block[4 + c], block[8 + c], block[12 + c]);
            c += 1;
        }
        r += 1;
    }
    out
}

/// Apply the 4×4 luma transform matrix to a row-major 4×4 input block by
/// multiplying the matrix into the block's **rows** (`X · M^T`).
///
/// This is the right-side mirror of [`apply_luma_transform_columns`]: where
/// that helper applies the pinned matrix [`LUMA_TRANSFORM_MATRIX`] against the
/// block's columns (the `M · X` pass), this one applies the *same* matrix
/// against the block's rows. Output position `(r, c)` is the dot product of
/// the block's row `r` with the matrix's row `c`:
///
/// ```text
///   out[r, c] = X[r, :] · M[c, :]
///             = sum over k of block[r * 4 + k] * LUMA_TRANSFORM_MATRIX[c][k]
/// ```
///
/// (which equals `(X · M^T)[r, c]`, since column `c` of `M^T` is row `c` of
/// `M`). Each output element therefore reuses the per-row dot product
/// [`apply_luma_transform_row`] with `matrix_row = LUMA_TRANSFORM_MATRIX[c]`
/// and the four samples drawn from row `r` of the block.
///
/// Like [`apply_luma_transform_columns`], this is a **single** matrix pass —
/// only `M` (pinned verbatim by the wiki spec) is involved. The full two-sided
/// `M · X · M^T` transform — composing this row pass with the column pass — is
/// NOT folded in here: the wiki spec's §"Macroblock transform and
/// dequantization" quotes the matrix under "Transform coefficients" but does
/// NOT enumerate the two-sided composition, so that derivation stays deferred
/// until the docs pin it.
///
/// The input `block` is laid out row-major (`block[r * 4 + c]` = sample at row
/// `r`, column `c`); the returned `[i32; 16]` is laid out the same way. The
/// unrounded i32 outputs feed into [`dequantize_coefficient`] for the
/// per-sample dequant step; this helper applies no shift, bias, or quantiser
/// scaling.
///
/// # Examples
///
/// A pure-DC row input (row 0 active, others zero) reproduces the shared
/// column-0 weight `13` in output column 0, and the row's own AC structure in
/// the other output columns:
///
/// ```
/// use oxideav_svq::svq3_dequant::apply_luma_transform_rows;
/// // Row 0 holds [a, 0, 0, 0] (block[0] = a, rest of row 0 = 0).
/// let mut block = [0i32; 16];
/// block[0] = 2; // (row 0, col 0)
/// let out = apply_luma_transform_rows(block);
/// // out[0, c] = block[0] * M[c][0] = 2 * 13 = 26 for every output column c,
/// // because column 0 of M is all 13.
/// assert_eq!(out[0], 26); // (0, 0)
/// assert_eq!(out[1], 26); // (0, 1)
/// assert_eq!(out[2], 26); // (0, 2)
/// assert_eq!(out[3], 26); // (0, 3)
/// // Rows 1..3 of the block are zero, so their outputs are zero.
/// assert_eq!(out[4], 0);
/// ```
#[inline]
#[must_use]
pub const fn apply_luma_transform_rows(block: [i32; 16]) -> [i32; 16] {
    // block layout: row-major 4×4 (block[r * 4 + c] = sample at (r, c)).
    let mut out = [0i32; 16];
    // out[r * 4 + c] = X[r, :] · M[c, :]
    //              = sum over k of block[r * 4 + k] * M[c][k]
    let mut r = 0;
    while r < 4 {
        let a = block[r * 4];
        let b = block[r * 4 + 1];
        let c_sample = block[r * 4 + 2];
        let d = block[r * 4 + 3];
        let mut c = 0;
        while c < 4 {
            out[r * 4 + c] = apply_luma_transform_row(LUMA_TRANSFORM_MATRIX[c], a, b, c_sample, d);
            c += 1;
        }
        r += 1;
    }
    out
}

/// Apply the full **two-sided** 4×4 luma transform `M · X · M^T` to a
/// row-major 4×4 input block.
///
/// This composes the two pinned single-sided passes already defined in this
/// module:
///
/// * [`apply_luma_transform_rows`] performs the right-side pass `X · M^T`
///   (the matrix multiplied into the block's rows), and
/// * [`apply_luma_transform_columns`] performs the left-side pass `M · (·)`
///   (the matrix multiplied into the block's columns).
///
/// Chaining them — columns-pass applied to the output of the rows-pass —
/// realises `M · (X · M^T) = M · X · M^T`. Both factors are the *same* matrix
/// `M` ([`LUMA_TRANSFORM_MATRIX`], pinned verbatim by the wiki spec's
/// §"Macroblock transform and dequantization"); no new matrix or constant is
/// introduced here. The composition order is the matrix-algebra associativity
/// of two already-pinned passes, not an additional spec fact.
///
/// Consistent with both single-sided passes, **no inter-pass shift, bias, or
/// quantiser scaling is applied** — the wiki spec lists the matrix under
/// "Transform coefficients" without enumerating any normalisation between the
/// two passes, so none is folded in. The unrounded i32 outputs feed the
/// per-coefficient dequant step ([`dequantize_coefficient`]); equivalently the
/// composition could be written `(M · X) · M^T` (columns then rows) — the two
/// orderings agree exactly for integer matrix multiplication, which the
/// [`tests`] module corroborates.
///
/// The input `block` is laid out row-major (`block[r * 4 + c]` = sample at row
/// `r`, column `c`); the returned `[i32; 16]` is laid out the same way.
///
/// # Examples
///
/// A pure-DC block (only `(0, 0)` non-zero) yields the rank-one outer product
/// of column 0 of `M` with itself, scaled by the DC sample — every output
/// element is `13 * 13 * block[0]`:
///
/// ```
/// use oxideav_svq::svq3_dequant::apply_luma_transform_2d;
/// let mut block = [0i32; 16];
/// block[0] = 1; // (row 0, col 0)
/// let out = apply_luma_transform_2d(block);
/// // Column 0 of M is all 13, so M · X · M^T for a single DC sample is the
/// // all-(13 * 13) matrix.
/// for v in out {
///     assert_eq!(v, 13 * 13);
/// }
/// ```
#[inline]
#[must_use]
pub const fn apply_luma_transform_2d(block: [i32; 16]) -> [i32; 16] {
    // M · (X · M^T): right-side rows pass first, then the left-side columns
    // pass. Both passes use the same pinned LUMA_TRANSFORM_MATRIX.
    apply_luma_transform_columns(apply_luma_transform_rows(block))
}

/// The literal `1538` multiplier of the luma DC secondary transform
/// (`docs/video/svq3/spec/04-dc-secondary-transform.md` §4.1 step 3).
///
/// spec/04 §4.2: 1538 is the value on the wire-format side of the
/// contract and must be used verbatim (it is 0.9 % away from the 1551
/// an exactly orthonormal secondary transform would need — that
/// observation explains the constant's size, it is not a formula to
/// re-derive it from).
pub const LUMA_DC_SECONDARY_SCALE: i32 = 1538;

/// The luma DC secondary transform of
/// `docs/video/svq3/spec/04-dc-secondary-transform.md` §4: given the
/// sixteen decoded levels of the separate luma DC block of an intra
/// 16×16 macroblock (already placed through the normal zigzag into
/// row-major order), produce the sixteen per-block DC terms `v_k`.
///
/// The pipeline is, verbatim from §4.1:
///
/// 1. dequantise with the **luma** quantiser (`level ×
///    DEQUANT_COEFF_TABLE[q]` — no chroma remap, no separate ladder);
/// 2. apply the ordinary core 4×4 inverse transform of §1 —
///    the same [`apply_luma_transform_2d`] kernel with its fused
///    `+0x80000, >> 20` normalisation (**not** a Hadamard: spec/04
///    §4.1 pins that no 4×4 Hadamard exists in the codec);
/// 3. multiply each of the sixteen results by
///    [`LUMA_DC_SECONDARY_SCALE`] = `1538`.
///
/// Result `v_k` (raster order `k = y·4 + x` over the transform
/// output) becomes coefficient position 0 of the *k*-th 4×4 luma
/// block, in raster order across the macroblock — equivalently, the
/// additive post-transform term `169 · v_k` in the fused store
/// ([`dequantize_transform_luma_block_with_dc`]).
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
#[inline]
#[must_use]
pub const fn luma_dc_secondary_transform(q: u32, dc_block: [i32; 16]) -> [i32; 16] {
    // Step 1: luma-quantiser dequant, no rounding (2²⁰ scale).
    let scaled = scale_luma_block_by_quantiser(q, dc_block);
    // Step 2: the core transform with its fused +0x80000 >> 20 store.
    let transformed = apply_luma_transform_2d(scaled);
    let mut out = [0i32; 16];
    let mut i = 0;
    while i < 16 {
        let t = (transformed[i] as i64 + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT;
        // Step 3: the literal 1538 (spec/04 §4.2 — verbatim contract).
        out[i] = sat_i32(t * LUMA_DC_SECONDARY_SCALE as i64);
        i += 1;
    }
    out
}

/// Scale a placed 4×4 luma coefficient block by the per-quantiser
/// dequant coefficient `DEQUANT_COEFF_TABLE[Q]`, in-place per element.
///
/// This is the dequant-multiply half of spec/01 Gap 2's fused
/// `out = (coeff·DEQUANT_COEFF_TABLE[Q] + dc + 0x80000) >> 20` store:
/// every placed coefficient `coeff` is multiplied by the quantiser
/// scale `DEQUANT_COEFF_TABLE[Q]` *before* the two-sided transform runs,
/// matching the binary's "dequantisation … and the inverse transform
/// are performed in the same pass". The additive `dc`, round bias
/// `0x80000`, and `>> 20` shift are applied *after* the transform by the
/// caller (see [`dequantize_transform_luma_block`]); this helper does
/// none of those.
///
/// The input `block` is row-major (`block[r * 4 + c]`); the output keeps
/// the same layout with each entry multiplied by the scale.
///
/// The caller must ensure `q < DEQUANT_COEFF_TABLE_LEN`.
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
#[inline]
#[must_use]
pub const fn scale_luma_block_by_quantiser(q: u32, block: [i32; 16]) -> [i32; 16] {
    let scale = DEQUANT_COEFF_TABLE[q as usize] as i64;
    let mut out = [0i32; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = sat_i32(block[i] as i64 * scale);
        i += 1;
    }
    out
}

/// Run the **full luma residual interleave** on a placed 4×4 luma
/// coefficient block: per-coefficient dequant-scale → two-sided
/// `M · X · M^T` transform → fused `+ dc + 0x80000 >> 20`.
///
/// This composes spec/01 Gap 2's pinned per-element formula
///
/// ```text
///   out[i] = ( transform( coeff · DEQUANT_COEFF_TABLE[Q] )[i]
///              + dc + 0x80000 ) >> 20
/// ```
///
/// in the order the binary realises it (the dequant multiply folded into
/// the same pass as the inverse transform, the single `>> 20` being the
/// only post-transform shift — no extra `>> 6`):
///
/// 1. [`scale_luma_block_by_quantiser`] multiplies every placed
///    coefficient by `DEQUANT_COEFF_TABLE[Q]`;
/// 2. [`apply_luma_transform_2d`] applies the two-sided `M · X · M^T`
///    transform (rows pass then columns pass over the pinned
///    [`LUMA_TRANSFORM_MATRIX`]); and
/// 3. each transformed sample is finalised with `(s + dc + 0x80000) >> 20`
///    ([`DEQUANT_ROUND`] / [`DEQUANT_SHIFT`]), where `dc` is the
///    additive override term (`0` for the common no-separate-DC luma
///    block — see [`dequantize_transform_luma_block`]).
///
/// The input `block` is the row-major placed coefficient grid from
/// [`crate::svq3_scan::place_4x4`]; the returned `[i32; 16]` is the
/// fully-dequantised, inverse-transformed **residual** ready for the
/// `Clip1(pred + residual)` writeback ([`crate::svq3_pred::reconstruct_4x4`]).
///
/// The caller must ensure `q < DEQUANT_COEFF_TABLE_LEN`.
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
#[inline]
#[must_use]
pub const fn dequantize_transform_luma_block_with_dc(
    q: u32,
    block: [i32; 16],
    dc: i64,
) -> [i32; 16] {
    // Stage 1: per-coefficient dequant multiply (same pass as transform).
    let scaled = scale_luma_block_by_quantiser(q, block);
    // Stage 2: two-sided M · X · M^T inverse transform.
    let transformed = apply_luma_transform_2d(scaled);
    // Stage 3: fused +dc +0x80000 >>20 — the single post-transform shift.
    let mut out = [0i32; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = sat_i32((transformed[i] as i64 + dc + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT);
        i += 1;
    }
    out
}

/// Run the luma residual interleave with no separate-DC override
/// (`dc = 0`), the common case for a 4×4-intra luma block whose DC is
/// carried inline in `block[0]` rather than in a separate DC stream.
///
/// Thin wrapper over [`dequantize_transform_luma_block_with_dc`] with
/// `dc = 0` — spec/01 Gap 2's `dc = 0 unless overridden`. Use the
/// `_with_dc` form for the intra-luma separate-DC branch, where the
/// caller supplies `dc = INTRA_LUMA_DC_SCALE · block[0]` (see
/// [`dequantize_intra_luma_dc`]) computed from the separate DC block.
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
#[inline]
#[must_use]
pub const fn dequantize_transform_luma_block(q: u32, block: [i32; 16]) -> [i32; 16] {
    dequantize_transform_luma_block_with_dc(q, block, 0)
}

/// Run the luma residual interleave for an **intra** luma 4×4 block
/// whose DC coefficient is carried inline (no separate DC block), using
/// the SVQ3-specific intra-luma DC scale rather than the general
/// per-coefficient dequant for the DC term.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
/// transform and dequantization": "For intra luma blocks without
/// separate DC coefficients block: `dc = 13 * 13 * 1538 * block[0]`".
/// The wiki's general dequant formula
/// `out = (coeff · svq3_dequant_coeff[Q] + dc + 0x80000) >> 20` then
/// uses this `dc` as the additive override term. So for an intra luma
/// block the DC contribution comes from [`INTRA_LUMA_DC_SCALE`] applied
/// to `block[0]` (via [`dequantize_intra_luma_dc`]), **not** from
/// `block[0]` running through the `coeff · svq3_dequant_coeff[Q]` AC
/// scale. This helper therefore:
///
/// 1. computes the DC override `dc = INTRA_LUMA_DC_SCALE · block[0]`;
/// 2. zeroes `block[0]` so the inline DC coefficient does not *also*
///    contribute through the general AC dequant + transform; and
/// 3. runs [`dequantize_transform_luma_block_with_dc`] with that `dc`.
///
/// The `dc` override is added to **every** transformed sample (it is the
/// post-transform additive term, exactly as the wiki formula writes),
/// which is the separable-transform's DC basis (`block[0]` projected
/// through the column-0 = `13` basis on both passes — the `13 · 13`
/// inside [`INTRA_LUMA_DC_SCALE`]).
///
/// The `block` argument is the row-major placed coefficient grid from
/// [`crate::svq3_scan::place_4x4`]. The returned `[i32; 16]` is the
/// dequantised, inverse-transformed residual.
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
#[inline]
#[must_use]
pub const fn dequantize_transform_intra_luma_block(q: u32, block: [i32; 16]) -> [i32; 16] {
    let dc = dequantize_intra_luma_dc(block[0]);
    let mut ac = block;
    ac[0] = 0;
    dequantize_transform_luma_block_with_dc(q, ac, dc)
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
        // Row-by-row corroboration against the spec/04 §1 measured
        // basis (third column ±13, the correction over the wiki
        // snapshot's ±1):
        //   13  17  13   7
        //   13   7 -13 -17
        //   13  -7 -13  17
        //   13 -17  13  -7
        assert_eq!(LUMA_TRANSFORM_MATRIX[0], [13, 17, 13, 7]);
        assert_eq!(LUMA_TRANSFORM_MATRIX[1], [13, 7, -13, -17]);
        assert_eq!(LUMA_TRANSFORM_MATRIX[2], [13, -7, -13, 17]);
        assert_eq!(LUMA_TRANSFORM_MATRIX[3], [13, -17, 13, -7]);
    }

    #[test]
    fn luma_transform_basis_vectors_share_squared_norm() {
        // spec/04 §1: every basis vector (matrix column) has squared
        // norm 4·13² = 17² + 2·7² + 17² = 676 — which the wiki's ±1
        // third column would violate (it gives 4).
        for col in 0..4 {
            let norm: i32 = LUMA_TRANSFORM_MATRIX
                .iter()
                .map(|row| row[col] * row[col])
                .sum();
            assert_eq!(norm, 676, "column {col}");
        }
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
    fn dequantize_intra_luma_dc_zero_input_is_zero() {
        assert_eq!(dequantize_intra_luma_dc(0), 0);
    }

    #[test]
    fn dequantize_intra_luma_dc_one_input_is_scale_value() {
        // The bare wiki-spec expression `13 * 13 * 1538 * block[0]`
        // with `block[0] = 1` returns the scale value itself
        // (= 259_922).
        assert_eq!(dequantize_intra_luma_dc(1), INTRA_LUMA_DC_SCALE as i64);
        assert_eq!(dequantize_intra_luma_dc(1), 259_922);
    }

    #[test]
    fn dequantize_intra_luma_dc_negative_one_input_is_negative_scale() {
        assert_eq!(dequantize_intra_luma_dc(-1), -(INTRA_LUMA_DC_SCALE as i64));
    }

    #[test]
    fn dequantize_intra_luma_dc_two_input_doubles_scale() {
        assert_eq!(dequantize_intra_luma_dc(2), 2 * INTRA_LUMA_DC_SCALE as i64);
    }

    #[test]
    fn chroma_quantiser_index_verbatim() {
        // tables/02-chroma-quantiser-index.csv: identity for 0..=17,
        // then 17,18,19,20,20,21,22,22,23,23,24,24,25,25.
        for q in 0..=17u32 {
            assert_eq!(chroma_quantiser_index(q), q, "identity at {q}");
        }
        let tail = [17, 18, 19, 20, 20, 21, 22, 22, 23, 23, 24, 24, 25, 25];
        for (i, &want) in tail.iter().enumerate() {
            let q = 18 + i as u32;
            assert_eq!(chroma_quantiser_index(q), want, "remap at {q}");
        }
    }

    #[test]
    fn chroma_quantiser_index_saturates_at_25() {
        // spec/04 §3: the effective chroma multiplier saturates at
        // ladder entry 25 (68745) while luma continues to 141533.
        assert_eq!(chroma_quantiser_index(31), 25);
        assert_eq!(
            DEQUANT_COEFF_TABLE[chroma_quantiser_index(31) as usize],
            68745
        );
        assert_eq!(DEQUANT_COEFF_TABLE[31], 141533);
    }

    #[test]
    fn chroma_dc_secondary_transform_measured_examples() {
        // spec/04 §2.2 measured input→output relations. The division
        // truncates toward zero: (−3, 0, 0, 0) gives −1s, not the −2s
        // an arithmetic shift would give.
        assert_eq!(
            chroma_dc_secondary_transform([-3, 0, 0, 0]),
            [-1, -1, -1, -1]
        );
        assert_eq!(chroma_dc_secondary_transform([3, 0, 0, 0]), [1, 1, 1, 1]);
        // (100, 20, −8, 3) → (115/2, 69/2, 125/2, 91/2) = (57, 34, 62, 45).
        assert_eq!(
            chroma_dc_secondary_transform([100, 20, -8, 3]),
            [57, 34, 62, 45]
        );
    }

    #[test]
    fn chroma_dc_secondary_transform_is_hadamard_halved() {
        // Cross-check against the H·X·Hᵀ closed form for even sums
        // (where truncation is exact).
        let c = [10, -4, 6, 2];
        let expected = [
            (c[0] + c[1] + c[2] + c[3]) / 2,
            (c[0] - c[1] + c[2] - c[3]) / 2,
            (c[0] + c[1] - c[2] - c[3]) / 2,
            (c[0] - c[1] - c[2] + c[3]) / 2,
        ];
        assert_eq!(chroma_dc_secondary_transform(c), expected);
    }

    #[test]
    fn dequantize_chroma_dc_levels_applies_remap_then_butterfly() {
        // q = 31 remaps to ladder entry 25 (68745). A single level at
        // c0 spreads uniformly: B_k = (level · 68745) / 2.
        let out = dequantize_chroma_dc_levels(31, [2, 0, 0, 0]);
        let expected = (2 * 68745) / 2;
        assert_eq!(out, [expected; 4]);
        // In the identity range the ladder entry is the luma one.
        let out0 = dequantize_chroma_dc_levels(0, [2, 0, 0, 0]);
        assert_eq!(out0, [3881; 4]);
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
        let dc = dequantize_intra_luma_dc(1) as i32;
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

    // ---- 4×4 luma transform application ----

    #[test]
    fn luma_transform_row_sums_weights_for_all_ones_column() {
        // Each row applied to [1, 1, 1, 1] returns the sum of its weights.
        for row in LUMA_TRANSFORM_MATRIX.iter() {
            let expected = row[0] + row[1] + row[2] + row[3];
            assert_eq!(apply_luma_transform_row(*row, 1, 1, 1, 1), expected);
        }
    }

    #[test]
    fn luma_transform_row_pure_dc_column_yields_thirteen_times_a() {
        // A column [a, 0, 0, 0] reduces every row to its shared col-0 weight
        // (13) times a.
        for row in LUMA_TRANSFORM_MATRIX.iter() {
            for a in [-3, -1, 0, 1, 4, 17] {
                assert_eq!(
                    apply_luma_transform_row(*row, a, 0, 0, 0),
                    LUMA_TRANSFORM_DC_COLUMN * a,
                );
            }
        }
    }

    #[test]
    fn luma_transform_row_explicit_dot_products() {
        // Worked examples against the column [1, 2, 3, 4].
        // Row 0 [13,17,13,7]:  13 + 34 + 39 + 28 = 114.
        assert_eq!(
            apply_luma_transform_row(LUMA_TRANSFORM_MATRIX[0], 1, 2, 3, 4),
            114,
        );
        // Row 1 [13,7,-13,-17]: 13 + 14 - 39 - 68 = -80.
        assert_eq!(
            apply_luma_transform_row(LUMA_TRANSFORM_MATRIX[1], 1, 2, 3, 4),
            -80,
        );
        // Row 2 [13,-7,-13,17]: 13 - 14 - 39 + 68 = 28.
        assert_eq!(
            apply_luma_transform_row(LUMA_TRANSFORM_MATRIX[2], 1, 2, 3, 4),
            28,
        );
        // Row 3 [13,-17,13,-7]: 13 - 34 + 39 - 28 = -10.
        assert_eq!(
            apply_luma_transform_row(LUMA_TRANSFORM_MATRIX[3], 1, 2, 3, 4),
            -10,
        );
    }

    #[test]
    fn luma_transform_row_is_linear_in_input() {
        let row = LUMA_TRANSFORM_MATRIX[1];
        let base = apply_luma_transform_row(row, 2, -3, 5, 7);
        // Doubling every input doubles the output.
        assert_eq!(apply_luma_transform_row(row, 4, -6, 10, 14), 2 * base);
        // Negating every input negates the output.
        assert_eq!(apply_luma_transform_row(row, -2, 3, -5, -7), -base);
    }

    #[test]
    fn luma_transform_columns_all_zero_block_yields_all_zero_output() {
        assert_eq!(apply_luma_transform_columns([0; 16]), [0; 16]);
    }

    #[test]
    fn luma_transform_columns_pure_dc_column_repeats_thirteen_a_down_rows() {
        // block[0] = a at (row 0, col 0); rest of column 0 zero. Each output
        // row's column 0 is 13 * a; every other output column is zero.
        let mut block = [0i32; 16];
        block[0] = 3;
        let out = apply_luma_transform_columns(block);
        for r in 0..4 {
            assert_eq!(out[r * 4], LUMA_TRANSFORM_DC_COLUMN * 3);
            assert_eq!(out[r * 4 + 1], 0);
            assert_eq!(out[r * 4 + 2], 0);
            assert_eq!(out[r * 4 + 3], 0);
        }
    }

    #[test]
    fn luma_transform_columns_single_column_active_matches_per_row_dot() {
        // Put a full column [1, 2, 3, 4]^T into column 2; every other column
        // stays zero. Output column 2 must equal each row's dot against that
        // column; all other output columns are zero.
        let mut block = [0i32; 16];
        block[2] = 1; // (0, 2)
        block[6] = 2; // (1, 2)
        block[10] = 3; // (2, 2)
        block[14] = 4; // (3, 2)
        let out = apply_luma_transform_columns(block);
        for r in 0..4 {
            let expected = apply_luma_transform_row(LUMA_TRANSFORM_MATRIX[r], 1, 2, 3, 4);
            assert_eq!(out[r * 4 + 2], expected);
            assert_eq!(out[r * 4], 0);
            assert_eq!(out[r * 4 + 1], 0);
            assert_eq!(out[r * 4 + 3], 0);
        }
    }

    #[test]
    fn luma_transform_columns_is_linear_in_input() {
        let mut block = [0i32; 16];
        for (i, slot) in block.iter_mut().enumerate() {
            *slot = (i as i32) - 8;
        }
        let base = apply_luma_transform_columns(block);
        let doubled: [i32; 16] = core::array::from_fn(|i| block[i] * 2);
        let out_doubled = apply_luma_transform_columns(doubled);
        for i in 0..16 {
            assert_eq!(out_doubled[i], 2 * base[i]);
        }
        let negated: [i32; 16] = core::array::from_fn(|i| -block[i]);
        let out_negated = apply_luma_transform_columns(negated);
        for i in 0..16 {
            assert_eq!(out_negated[i], -base[i]);
        }
    }

    #[test]
    fn luma_transform_columns_decomposes_into_per_row_per_column_dots() {
        // The full helper must agree with the row helper applied
        // position-by-position over an arbitrary block.
        let mut block = [0i32; 16];
        for (i, slot) in block.iter_mut().enumerate() {
            *slot = ((i * 7 + 3) % 13) as i32 - 6;
        }
        let out = apply_luma_transform_columns(block);
        for r in 0..4 {
            for c in 0..4 {
                let expected = apply_luma_transform_row(
                    LUMA_TRANSFORM_MATRIX[r],
                    block[c],
                    block[4 + c],
                    block[8 + c],
                    block[12 + c],
                );
                assert_eq!(out[r * 4 + c], expected);
            }
        }
    }

    #[test]
    fn luma_transform_columns_const_evaluable_in_static_context() {
        const IN: [i32; 16] = {
            let mut b = [0i32; 16];
            b[0] = 1;
            b
        };
        const OUT: [i32; 16] = apply_luma_transform_columns(IN);
        // Pure-DC column 0 → 13 down output column 0, zeros elsewhere.
        assert_eq!(OUT[0], 13);
        assert_eq!(OUT[4], 13);
        assert_eq!(OUT[8], 13);
        assert_eq!(OUT[12], 13);
        assert_eq!(OUT[1], 0);
    }

    #[test]
    fn luma_transform_rows_doc_example() {
        // Row 0 = [2, 0, 0, 0]; output row 0 = block[0] * M[c][0] = 2*13 = 26
        // for every output column c (column 0 of M is all 13). Other rows zero.
        let mut block = [0i32; 16];
        block[0] = 2;
        let out = apply_luma_transform_rows(block);
        assert_eq!(out[0], 26);
        assert_eq!(out[1], 26);
        assert_eq!(out[2], 26);
        assert_eq!(out[3], 26);
        for &v in &out[4..] {
            assert_eq!(v, 0);
        }
    }

    #[test]
    fn luma_transform_rows_single_active_row_picks_matrix_row_dot() {
        // A single active block row r reproduces, in output row r, the dot
        // product of that row's samples with each matrix row in turn.
        let samples = [3i32, -5, 7, -2];
        for active in 0..4 {
            let mut block = [0i32; 16];
            block[active * 4..active * 4 + 4].copy_from_slice(&samples);
            let out = apply_luma_transform_rows(block);
            for c in 0..4 {
                let expected = apply_luma_transform_row(
                    LUMA_TRANSFORM_MATRIX[c],
                    samples[0],
                    samples[1],
                    samples[2],
                    samples[3],
                );
                assert_eq!(out[active * 4 + c], expected, "row {active}, col {c}");
            }
            // Every other output row is zero.
            for r in 0..4 {
                if r != active {
                    for c in 0..4 {
                        assert_eq!(out[r * 4 + c], 0, "leak into row {r}");
                    }
                }
            }
        }
    }

    #[test]
    fn luma_transform_rows_matches_explicit_x_mt_definition() {
        // out[r, c] = sum_k block[r*4 + k] * M[c][k] over an arbitrary block.
        let mut block = [0i32; 16];
        for (i, slot) in block.iter_mut().enumerate() {
            *slot = ((i * 5 + 2) % 11) as i32 - 5;
        }
        let out = apply_luma_transform_rows(block);
        for r in 0..4 {
            for c in 0..4 {
                let mut expected = 0i32;
                for k in 0..4 {
                    expected += block[r * 4 + k] * LUMA_TRANSFORM_MATRIX[c][k];
                }
                assert_eq!(out[r * 4 + c], expected, "({r}, {c})");
            }
        }
    }

    #[test]
    fn luma_transform_rows_is_columns_of_the_transposed_input() {
        // X · M^T transposed equals M · X^T: applying the row helper to X and
        // transposing must equal applying the column helper to X^T.
        let mut block = [0i32; 16];
        for (i, slot) in block.iter_mut().enumerate() {
            *slot = (i as i32) - 8;
        }
        let mut transposed = [0i32; 16];
        for r in 0..4 {
            for c in 0..4 {
                transposed[c * 4 + r] = block[r * 4 + c];
            }
        }
        let rows_out = apply_luma_transform_rows(block);
        let cols_of_transpose = apply_luma_transform_columns(transposed);
        for r in 0..4 {
            for c in 0..4 {
                // rows_out[r][c] = X[r,:]·M[c,:] = (M·X^T)[c][r] = cols_of_transpose[c][r]
                assert_eq!(
                    rows_out[r * 4 + c],
                    cols_of_transpose[c * 4 + r],
                    "({r},{c})"
                );
            }
        }
    }

    #[test]
    fn luma_transform_rows_const_evaluable_in_static_context() {
        const IN: [i32; 16] = {
            let mut b = [0i32; 16];
            b[0] = 1;
            b
        };
        const OUT: [i32; 16] = apply_luma_transform_rows(IN);
        // Pure-DC row 0 → 13 across output row 0, zeros elsewhere.
        assert_eq!(OUT[0], 13);
        assert_eq!(OUT[1], 13);
        assert_eq!(OUT[2], 13);
        assert_eq!(OUT[3], 13);
        assert_eq!(OUT[4], 0);
    }

    // ----- Two-sided luma transform M · X · M^T -----------------------------

    /// Brute-force reference: triple-loop `M · X · M^T` for a row-major 4×4
    /// block, computed straight from [`LUMA_TRANSFORM_MATRIX`]. Used only to
    /// corroborate the composed helper; it duplicates no production code.
    fn reference_luma_2d(block: [i32; 16]) -> [i32; 16] {
        let m = LUMA_TRANSFORM_MATRIX;
        let mut out = [0i32; 16];
        for i in 0..4 {
            for j in 0..4 {
                let mut acc = 0i32;
                for p in 0..4 {
                    for q in 0..4 {
                        // (M · X · M^T)[i][j] = sum_{p,q} M[i][p] * X[p][q] * M[j][q]
                        acc += m[i][p] * block[p * 4 + q] * m[j][q];
                    }
                }
                out[i * 4 + j] = acc;
            }
        }
        out
    }

    #[test]
    fn luma_2d_doc_example_pure_dc() {
        let mut block = [0i32; 16];
        block[0] = 1;
        let out = apply_luma_transform_2d(block);
        // M · X · M^T for a single DC sample is the all-(13*13) matrix.
        for v in out {
            assert_eq!(v, 13 * 13);
        }
    }

    #[test]
    fn luma_2d_matches_brute_force_reference() {
        for block in [
            {
                let mut b = [0i32; 16];
                b[0] = 5;
                b
            },
            {
                let mut b = [0i32; 16];
                let mut k = 0;
                while k < 16 {
                    b[k] = k as i32 - 7;
                    k += 1;
                }
                b
            },
            [3, -1, 4, -1, 5, -9, 2, -6, 5, -3, 5, -8, 9, -7, 9, -3],
            [
                -100, 100, -50, 50, 25, -25, 12, -12, 6, -6, 3, -3, 1, -1, 0, 0,
            ],
        ] {
            assert_eq!(apply_luma_transform_2d(block), reference_luma_2d(block));
        }
    }

    #[test]
    fn luma_2d_order_independent_columns_then_rows() {
        // (M · X) · M^T must equal M · (X · M^T) for integer matrix multiply.
        for block in [
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            [-5, 0, 17, -3, 8, 8, -8, 1, 0, 0, 4, -4, 100, -100, 2, -2],
        ] {
            let rows_then_cols = apply_luma_transform_2d(block);
            let cols_then_rows = apply_luma_transform_rows(apply_luma_transform_columns(block));
            assert_eq!(rows_then_cols, cols_then_rows);
        }
    }

    #[test]
    fn luma_2d_is_linear_in_input() {
        let x = [
            1, -2, 3, -4, 5, -6, 7, -8, 9, -10, 11, -12, 13, -14, 15, -16,
        ];
        let y = [16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1];
        let mut sum = [0i32; 16];
        for k in 0..16 {
            sum[k] = x[k] + y[k];
        }
        let tx = apply_luma_transform_2d(x);
        let ty = apply_luma_transform_2d(y);
        let tsum = apply_luma_transform_2d(sum);
        for k in 0..16 {
            assert_eq!(tsum[k], tx[k] + ty[k]);
        }
    }

    #[test]
    fn luma_2d_const_evaluable_in_static_context() {
        const IN: [i32; 16] = {
            let mut b = [0i32; 16];
            b[0] = 1;
            b
        };
        const OUT: [i32; 16] = apply_luma_transform_2d(IN);
        assert_eq!(OUT[0], 13 * 13);
        assert_eq!(OUT[15], 13 * 13);
    }

    // ---- Luma residual interleave (spec/01 Gap 2) ----------------------

    #[test]
    fn scale_luma_block_multiplies_every_entry() {
        let q = 7;
        let scale = DEQUANT_COEFF_TABLE[q as usize] as i32;
        let mut block = [0i32; 16];
        for (i, b) in block.iter_mut().enumerate() {
            *b = (i as i32) - 8; // mix of signs
        }
        let out = scale_luma_block_by_quantiser(q, block);
        for i in 0..16 {
            assert_eq!(out[i], block[i] * scale, "entry {i}");
        }
    }

    #[test]
    fn luma_residual_all_zero_input_is_zero() {
        // Zero coefficients → transform of zero is zero → (0 + 0 + round)
        // >> 20 = 0 for round = 0x80000 (< 2^20).
        for q in [0u32, 12, 31] {
            assert_eq!(dequantize_transform_luma_block(q, [0i32; 16]), [0i32; 16]);
        }
    }

    #[test]
    fn luma_residual_matches_explicit_stage_composition() {
        // The composed pipeline must equal the explicit
        // scale → transform → (+0x80000 >>20) stage chain.
        let q = 9;
        let mut block = [0i32; 16];
        for (i, b) in block.iter_mut().enumerate() {
            *b = ((i as i32) * 3 - 17) % 11;
        }
        let composed = dequantize_transform_luma_block(q, block);

        let scaled = scale_luma_block_by_quantiser(q, block);
        let transformed = apply_luma_transform_2d(scaled);
        let mut expected = [0i32; 16];
        for i in 0..16 {
            expected[i] = (transformed[i] + DEQUANT_ROUND) >> DEQUANT_SHIFT;
        }
        assert_eq!(composed, expected);
    }

    #[test]
    fn luma_residual_pure_dc_is_flat_block() {
        // A pure-DC placed block (only block[0] non-zero) transforms to
        // the all-(13·13·coeff·scale) matrix (Gap 2 rank-one outer
        // product), so after the uniform +0x80000 >>20 every output
        // element is identical.
        let q = 14;
        let mut block = [0i32; 16];
        block[0] = 5;
        let out = dequantize_transform_luma_block(q, block);
        for w in out.iter() {
            assert_eq!(*w, out[0], "pure-DC residual must be flat");
        }
        // Cross-check the exact value: 13·13·coeff·scale then round/shift.
        let scale = DEQUANT_COEFF_TABLE[q as usize] as i32;
        let expected = (13 * 13 * 5 * scale + DEQUANT_ROUND) >> DEQUANT_SHIFT;
        assert_eq!(out[0], expected);
    }

    #[test]
    fn luma_residual_dc_override_shifts_every_output() {
        // The separate-DC `dc` override is added to every transformed
        // sample before the shift; with a transform output of zero
        // (zero coefficients) the residual is uniformly
        // (dc + 0x80000) >> 20.
        let q = 3;
        let dc = 7i64 << 20; // a clean multiple to make the shift exact
        let out = dequantize_transform_luma_block_with_dc(q, [0i32; 16], dc);
        let expected = ((dc + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT) as i32;
        for w in out.iter() {
            assert_eq!(*w, expected);
        }
    }

    #[test]
    fn luma_residual_dc_zero_matches_no_dc_wrapper() {
        let q = 20;
        let mut block = [0i32; 16];
        for (i, b) in block.iter_mut().enumerate() {
            *b = (i as i32) - 6;
        }
        assert_eq!(
            dequantize_transform_luma_block(q, block),
            dequantize_transform_luma_block_with_dc(q, block, 0),
        );
    }

    #[test]
    fn luma_residual_const_evaluable() {
        const OUT: [i32; 16] = dequantize_transform_luma_block(10, {
            let mut b = [0i32; 16];
            b[0] = 1;
            b
        });
        assert_eq!(OUT[0], OUT[15]); // pure-DC flat
    }

    #[test]
    #[should_panic]
    fn luma_residual_panics_on_out_of_range_quantiser() {
        let _ = dequantize_transform_luma_block(DEQUANT_COEFF_TABLE_LEN as u32, [1i32; 16]);
    }

    #[test]
    fn intra_luma_dc_only_block_lifts_uniformly() {
        // An intra luma block with only block[0] non-zero: the DC term
        // is INTRA_LUMA_DC_SCALE * block[0] added to every transformed
        // sample (which is zero since AC is zeroed). Result is uniformly
        // (INTRA_LUMA_DC_SCALE*block0 + 0x80000) >> 20.
        let q = 12;
        let block0 = 4;
        let mut block = [0i32; 16];
        block[0] = block0;
        let out = dequantize_transform_intra_luma_block(q, block);
        let dc = (INTRA_LUMA_DC_SCALE * block0) as i64;
        let expected = ((dc + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT) as i32;
        for w in out.iter() {
            assert_eq!(*w, expected);
        }
    }

    #[test]
    fn intra_luma_block_uses_special_dc_scale_not_ac_scale() {
        // The intra-luma path must differ from the general AC path when
        // block[0] is non-zero (the DC uses INTRA_LUMA_DC_SCALE, the AC
        // path uses DEQUANT_COEFF_TABLE[Q]). Pick a Q where the two
        // scales differ.
        let q = 5;
        let mut block = [0i32; 16];
        block[0] = 3;
        let intra = dequantize_transform_intra_luma_block(q, block);
        let general = dequantize_transform_luma_block(q, block);
        assert_ne!(intra, general);
    }

    #[test]
    fn intra_luma_ac_only_block_matches_general_path() {
        // With block[0] == 0 the special DC term is zero, so the intra
        // path reduces exactly to the general (dc = 0) path.
        let q = 18;
        let mut block = [0i32; 16];
        block[5] = 7;
        block[11] = -3;
        assert_eq!(
            dequantize_transform_intra_luma_block(q, block),
            dequantize_transform_luma_block(q, block),
        );
    }

    #[test]
    fn intra_luma_block_const_evaluable() {
        const OUT: [i32; 16] = dequantize_transform_intra_luma_block(10, {
            let mut b = [0i32; 16];
            b[0] = 2;
            b
        });
        // Pure intra-DC → flat block.
        assert_eq!(OUT[0], OUT[15]);
    }

    /// Hostile-magnitude coefficients (wire-reachable: the Golomb
    /// walkers admit values up to `code >> 4` ≈ 2^28, and placed
    /// grids are arbitrary `i32` at the API boundary) must saturate
    /// through every dequant / transform helper instead of
    /// overflowing. Found by `fuzz/fuzz_targets/svq3_mb_layer` (i32
    /// multiply overflow in the dequant-scale pass).
    #[test]
    fn hostile_coefficients_saturate_instead_of_overflowing() {
        // The intra-luma DC scale is exact in 64-bit even at the i32
        // extremes.
        assert_eq!(
            dequantize_intra_luma_dc(i32::MAX),
            INTRA_LUMA_DC_SCALE as i64 * i32::MAX as i64
        );
        // The >>20 shift precedes the saturation, so even i32-extreme
        // inputs land back in-domain — exactly the widened evaluation
        // of the spec formula (a 32-bit evaluation would overflow).
        let scale31 = DEQUANT_COEFF_TABLE[31] as i64;
        assert_eq!(
            dequantize_coefficient(31, i32::MAX, i32::MAX),
            ((i32::MAX as i64 * scale31 + i32::MAX as i64 + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT)
                as i32
        );
        assert_eq!(
            dequantize_coefficient(31, i32::MIN, i32::MIN),
            ((i32::MIN as i64 * scale31 + i32::MIN as i64 + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT)
                as i32
        );
        assert_eq!(
            finalise_dc(i32::MAX),
            ((i32::MAX as i64 + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT) as i32
        );
        assert_eq!(
            apply_luma_transform_row(LUMA_TRANSFORM_MATRIX[0], i32::MAX, i32::MAX, 0, 0),
            i32::MAX
        );

        // Full block pipelines at the extremes stay panic-free and
        // land saturated (the downstream Clip1 writeback bounds the
        // reconstruction regardless of the exact saturated value).
        let hostile = [i32::MAX; 16];
        let _ = dequantize_transform_luma_block(31, hostile);
        let _ = dequantize_transform_luma_block_with_dc(31, hostile, i64::MIN / 4);
        let _ = dequantize_transform_intra_luma_block(31, [i32::MIN; 16]);
        let _ = dequantize_chroma_dc_levels(31, [i32::MIN, i32::MAX, i32::MIN, i32::MAX]);
        let _ = luma_dc_secondary_transform(31, hostile);

        // In-domain results are bit-identical to the plain 32-bit
        // evaluation of the spec formulas.
        let q = 17;
        let coeff = 1023;
        let scale = DEQUANT_COEFF_TABLE[q as usize] as i32;
        assert_eq!(
            dequantize_coefficient(q, coeff, 0),
            (coeff * scale + DEQUANT_ROUND) >> DEQUANT_SHIFT
        );
    }

    // ---- spec/04 §1 measured basis + §4 luma DC secondary transform ----

    /// Run one dequantised block through the core transform's fused
    /// `+0x80000 >> 20` store (the spec/04 §1 measurement harness).
    fn transform_and_round(block: [i32; 16]) -> [i32; 16] {
        let t = apply_luma_transform_2d(block);
        core::array::from_fn(|i| ((t[i] as i64 + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT) as i32)
    }

    #[test]
    fn measured_basis_single_coefficient_responses() {
        // spec/04 §1: feeding a single coefficient of value 2²⁰ into
        // the transform and reading back all sixteen outputs.
        let unit = 1i32 << 20;

        // Position 0 → uniform 169 = 13·13.
        let mut b = [0i32; 16];
        b[0] = unit;
        assert_eq!(transform_and_round(b), [169; 16]);

        // Position 1 → rows all 221, 91, −91, −221 = 13·(17, 7, −7, −17).
        let mut b = [0i32; 16];
        b[1] = unit;
        let out = transform_and_round(b);
        for r in 0..4 {
            assert_eq!(&out[r * 4..r * 4 + 4], &[221, 91, -91, -221], "row {r}");
        }

        // Position 2 → rows all 169, −169, −169, 169 = 13·13·(1, −1, −1, 1)
        // — the direct measurement that the third basis vector is
        // 13·(1, −1, −1, 1), not (1, −1, −1, 1).
        let mut b = [0i32; 16];
        b[2] = unit;
        let out = transform_and_round(b);
        for r in 0..4 {
            assert_eq!(&out[r * 4..r * 4 + 4], &[169, -169, -169, 169], "row {r}");
        }

        // Position 3 → rows all 91, −221, 221, −91 = 13·(7, −17, 17, −7).
        let mut b = [0i32; 16];
        b[3] = unit;
        let out = transform_and_round(b);
        for r in 0..4 {
            assert_eq!(&out[r * 4..r * 4 + 4], &[91, -221, 221, -91], "row {r}");
        }

        // Position 5 → outer product of (17, 7, −7, −17) with itself.
        let mut b = [0i32; 16];
        b[5] = unit;
        let out = transform_and_round(b);
        let v = [17i32, 7, -7, -17];
        for r in 0..4 {
            for c in 0..4 {
                assert_eq!(out[r * 4 + c], v[r] * v[c], "({r},{c})");
            }
        }

        // Position 15 → outer product of (7, −17, 17, −7) with itself.
        let mut b = [0i32; 16];
        b[15] = unit;
        let out = transform_and_round(b);
        let w = [7i32, -17, 17, -7];
        for r in 0..4 {
            for c in 0..4 {
                assert_eq!(out[r * 4 + c], w[r] * w[c], "({r},{c})");
            }
        }
    }

    #[test]
    fn luma_dc_secondary_scale_is_verbatim_1538() {
        // spec/04 §4.2: 1538 is the wire-format contract, used verbatim
        // (not the 1551 an exactly orthonormal cascade would need).
        assert_eq!(LUMA_DC_SECONDARY_SCALE, 1538);
        assert_eq!(LUMA_DC_SECONDARY_SCALE, INTRA_LUMA_DC_SCALE_TAIL);
    }

    #[test]
    fn luma_dc_secondary_transform_zero_is_zero() {
        for q in [0u32, 15, 31] {
            assert_eq!(luma_dc_secondary_transform(q, [0; 16]), [0; 16]);
        }
    }

    #[test]
    fn luma_dc_secondary_transform_single_dc_level() {
        // A single level at position 0: T is uniform
        // (169·level·dequant[q] + 0x80000) >> 20, and v = 1538·T.
        let q = 4u32;
        let level = 3i32;
        let mut b = [0i32; 16];
        b[0] = level;
        let out = luma_dc_secondary_transform(q, b);
        let scale = DEQUANT_COEFF_TABLE[q as usize] as i64;
        let t = ((169 * level as i64 * scale + DEQUANT_ROUND as i64) >> DEQUANT_SHIFT) as i32;
        assert_eq!(out, [1538 * t; 16]);
    }

    #[test]
    fn luma_dc_secondary_transform_uses_luma_quantiser() {
        // spec/04 §4.1: the DC block is dequantised with the luma
        // quantiser — no chroma remap. At q = 31 the luma ladder entry
        // (141533) differs from the remapped chroma one (68745), so the
        // two paths must diverge.
        let mut b = [0i32; 16];
        b[0] = 2;
        let luma = luma_dc_secondary_transform(31, b);
        let mut remapped = [0i32; 16];
        remapped[0] = 2;
        let chroma_style = luma_dc_secondary_transform(chroma_quantiser_index(31), remapped);
        assert_ne!(luma, chroma_style);
    }

    #[test]
    fn dc_additive_form_equals_position_zero_placement() {
        // The reconstruction layer's additive form 169·v must equal
        // placing v at coefficient position 0 of a dequantised block
        // and running the transform (column 0 of the basis is
        // uniformly 13).
        for v in [1i32, -7, 260, 1538] {
            let mut placed = [0i32; 16];
            placed[0] = v;
            let via_placement = transform_and_round(placed);
            let via_additive = dequantize_transform_luma_block_with_dc(0, [0; 16], 169 * v as i64);
            // The placement path multiplies by no quantiser scale
            // (already dequantised), so compare against the additive
            // path with zero coefficients (scale irrelevant).
            assert_eq!(via_placement, via_additive, "v = {v}");
        }
    }
}
