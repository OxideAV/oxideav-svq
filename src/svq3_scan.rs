//! SVQ3 per-block coefficient placement (scan-order infrastructure).
//!
//! ## Provenance
//!
//! Round 233 lands the per-block placement step that connects the
//! Golomb-decoded `(run, value)` coefficient stream produced by
//! [`crate::svq3_coeff`] to the 2D block matrix consumed by the
//! dequantization arithmetic in [`crate::svq3_dequant`]. The placement
//! step is described in `docs/video/svq3/wiki/Sorenson_Video_3.wiki`
//! §"Macroblock layer" / §"Coefficient decoding" — the wiki spec notes
//! that coefficients are stored in 4×4 (sub)blocks "except for chroma
//! DCs which are stored in 2×2 blocks", and that the per-block
//! `(run, value)` stream is consumed in **scan order**: each decoded
//! coefficient is placed at the next-non-zero scan position after the
//! current cursor advances `run` zero positions.
//!
//! ## Scope of round 233 (deliberately narrow)
//!
//! The wiki spec exposes three scan-order regimes:
//!
//! * **2×2 chroma DC** — four positions only. The wiki spec does not
//!   draw a dezigzag picture for the 2×2 case; the only natural order
//!   is row-major / column-major / diagonal, and for 2×2 those all
//!   collapse to the same four-entry list once the spec's
//!   §"Macroblock transform and dequantization" 2×2 chroma DC
//!   transform matrix `[[8, 8], [8, -8]]` is applied. Round 233
//!   transcribes the row-major order
//!   [`CHROMA_DC_2X2_SCAN`] (`(0,0) (0,1) (1,0) (1,1)`) as the
//!   placement order — this is the only ambiguity-free 2×2 scan and is
//!   what the chroma DC `(run, value)` walker emits.
//!
//! * **4×4 normal-zigzag (default case)** and **4×4 alt-scan** — both
//!   16-position scan arrays are now pinned by
//!   `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 1,
//!   which recovers them as two adjacent live tables in the Sorenson
//!   Video QuickTime Component's `.data` segment (file offsets
//!   `0x7e5a8` / `0x7e5b8`), cross-checked against the wiki's
//!   §"Macroblock layer" two-half alt-scan structure. They land here as
//!   [`NORMAL_ZIGZAG_4X4_SCAN`] / [`ALT_SCAN_4X4_SCAN`], consumed via
//!   [`place_coefficients_in_scan_order`] (or the
//!   [`place_4x4_normal_zigzag`] / [`place_4x4_alt_scan`] /
//!   [`place_4x4`] wrappers). Per spec/01 Gap 1 the alt-scan is selected
//!   **only for a luma 4×4-intra block when the slice quantiser is < 24**
//!   ([`select_4x4_scan`]); every other 4×4 block uses the normal
//!   zigzag.
//!
//! ## Placement contract
//!
//! [`place_coefficients_in_scan_order`] walks a slice of
//! [`crate::svq3_coeff::Coefficient`] in stream order, advancing a
//! scan-position cursor by `coeff.run + 1` per non-zero coefficient,
//! and writes `coeff.value` at the scan-order position `cursor + run`.
//! The function is generic over the destination's flat capacity and
//! returns a fixed-size flat array, leaving the 2D reshape to the
//! caller (the dequant step wants the 1D placed list; the IDCT step
//! wants the 2D reshape).
//!
//! This sits structurally between [`crate::svq3_coeff`] (which decodes
//! the `(run, value)` pairs) and [`crate::svq3_dequant`] (which applies
//! per-coefficient dequant). The dezigzag-to-2D-matrix reshape that
//! the wiki picture depicts is left to the round that pins the 4×4
//! scan-order array.
//!
//! ## Open work
//!
//! * The placement output is still not wired into
//!   [`crate::svq3_dequant`] — the per-block caller will perform that
//!   wiring (and the 2D reshape) once the macroblock decode loop that
//!   drives both is assembled.

use crate::svq3_coeff::Coefficient;

/// Number of coefficients in a 2×2 chroma DC block.
///
/// Mirrors [`crate::svq3_coeff::COEFFS_PER_CHROMA_DC_BLOCK`]; surfaced
/// here as a placement-side capacity constant so consumers of the
/// scan module do not need to depend on the coefficient-walker module
/// for the same length.
pub const CHROMA_DC_2X2_LEN: usize = 4;

/// Number of coefficients in a 4×4 (luma / chroma AC) block.
///
/// Mirrors [`crate::svq3_coeff::COEFFS_PER_4X4_BLOCK`]; surfaced here
/// as a placement-side capacity constant for consistency with
/// [`CHROMA_DC_2X2_LEN`].
pub const FULL_4X4_LEN: usize = 16;

/// 4×4 **normal-zigzag** scan order (the default case).
///
/// Pinned by `docs/video/svq3/spec/01-reconstruction-composition.md`
/// Gap 1, which recovers it as a live 16-byte table in the Sorenson
/// Video QuickTime Component's `.data` segment (file offset `0x7e5a8` /
/// VA `0x67dee5a8`), dereferenced directly inside the dequant/reorder
/// routine. Each entry is the **destination 4×4 raster index**
/// (`row * 4 + col`) for the n-th decoded coefficient. This is the
/// default reorder map; the [`ALT_SCAN_4X4_SCAN`] is used only for the
/// luma-4×4-intra-low-quantiser case (see [`select_4x4_scan`]).
pub const NORMAL_ZIGZAG_4X4_SCAN: [usize; FULL_4X4_LEN] =
    [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// 4×4 **alternate-scan** order.
///
/// Pinned by `docs/video/svq3/spec/01-reconstruction-composition.md`
/// Gap 1, recovered as the table adjacent to [`NORMAL_ZIGZAG_4X4_SCAN`]
/// in the same `.data` segment (file offset `0x7e5b8` / VA
/// `0x67dee5b8`), passed by pointer into the coefficient reader and
/// selected conditionally. Each entry is the **destination 4×4 raster
/// index** (`row * 4 + col`) for the n-th decoded coefficient.
///
/// The wiki §"Macroblock layer" notes the alt-scan block is "coded in
/// two parts of up to eight coefficients corresponding to each
/// half-scan"; this array splits exactly into two 8-entry halves
/// (`[0, 1, 2, 6, 10, 3, 7, 11]` then `[4, 8, 5, 9, 12, 13, 14, 15]`),
/// confirming the two-half structure ([`ALT_SCAN_4X4_HALF_LEN`]).
pub const ALT_SCAN_4X4_SCAN: [usize; FULL_4X4_LEN] =
    [0, 1, 2, 6, 10, 3, 7, 11, 4, 8, 5, 9, 12, 13, 14, 15];

/// Number of coefficients in each half-scan of the 4×4 alternate scan.
///
/// Per the wiki §"Macroblock layer" the alt-scan block is coded in two
/// parts of up to eight coefficients; [`ALT_SCAN_4X4_SCAN`] splits at
/// this length into its two recognised halves.
pub const ALT_SCAN_4X4_HALF_LEN: usize = 8;

/// Quantiser threshold below which the luma-4×4-intra alternate scan is
/// selected.
///
/// Per `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 1,
/// the alternate scan ([`ALT_SCAN_4X4_SCAN`]) is used **only for luma
/// blocks in a 4×4-intra macroblock when the slice quantiser is
/// `< 24`**; otherwise the normal zigzag ([`NORMAL_ZIGZAG_4X4_SCAN`]) is
/// used. See [`select_4x4_scan`].
pub const ALT_SCAN_QUANTISER_THRESHOLD: u32 = 24;

/// Select the 4×4 coefficient scan order for a luma block.
///
/// Per `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 1,
/// the alternate scan is used **only** when the block is a luma block of
/// a 4×4-intra macroblock *and* the slice quantiser is strictly below
/// [`ALT_SCAN_QUANTISER_THRESHOLD`] (`< 24`). Every other case — inter
/// blocks, 16×16-intra blocks, chroma AC blocks, or any block at
/// `quantiser >= 24` — uses the normal zigzag.
///
/// * `is_luma_4x4_intra` — the block belongs to a 4×4-intra MB's luma
///   plane (the only regime where the alt-scan applies).
/// * `quantiser` — the slice quantiser `Q`.
pub const fn select_4x4_scan(
    is_luma_4x4_intra: bool,
    quantiser: u32,
) -> &'static [usize; FULL_4X4_LEN] {
    if is_luma_4x4_intra && quantiser < ALT_SCAN_QUANTISER_THRESHOLD {
        &ALT_SCAN_4X4_SCAN
    } else {
        &NORMAL_ZIGZAG_4X4_SCAN
    }
}

/// 2×2 chroma DC scan order — row-major.
///
/// The wiki spec's §"Coefficient decoding" notes that "chroma DCs are
/// stored in 2×2 blocks" but does not draw a dezigzag picture for the
/// 2×2 case. For a 2×2 block the row-major / column-major / diagonal
/// scan orders all enumerate the same four positions; the row-major
/// order
/// (`(row, col) = (0,0), (0,1), (1,0), (1,1)`) is the only one that
/// matches the §"Macroblock transform and dequantization" 2×2 chroma
/// DC transform matrix `[[8, 8], [8, -8]]` indexing convention.
///
/// Each entry is a flat-index into a 2×2 block stored in row-major
/// order (so the matrix's `(r, c)` is at flat index `r * 2 + c`). For
/// the 2×2 row-major order, the flat-index list is the identity
/// `[0, 1, 2, 3]`.
pub const CHROMA_DC_2X2_SCAN: [usize; CHROMA_DC_2X2_LEN] = [0, 1, 2, 3];

/// Convert a 2×2 matrix position `(row, col)` to its flat index in a
/// row-major store.
///
/// Returns `None` if either coordinate is out of range. This is the
/// inverse mapping consumers can use to interpret entries of
/// [`CHROMA_DC_2X2_SCAN`] as `(row, col)` pairs.
pub const fn chroma_dc_2x2_flat_index(row: usize, col: usize) -> Option<usize> {
    if row >= 2 || col >= 2 {
        return None;
    }
    Some(row * 2 + col)
}

/// Convert a 2×2 flat index back to its `(row, col)` matrix position.
///
/// Returns `None` if the flat index is out of range.
pub const fn chroma_dc_2x2_matrix_position(flat_index: usize) -> Option<(usize, usize)> {
    if flat_index >= CHROMA_DC_2X2_LEN {
        return None;
    }
    Some((flat_index / 2, flat_index % 2))
}

/// Errors produced by the per-block placement helpers in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanError {
    /// The cumulative `(run + 1)` advance of the input coefficient
    /// stream would push the placement cursor past the destination's
    /// capacity. Carries the would-be cursor position (post-advance).
    OutOfRange(usize),
    /// The provided scan-order table contains a flat-index entry that
    /// is itself past the destination's capacity. Carries the offending
    /// scan-order entry. Indicates a malformed scan-order array, not a
    /// malformed input stream.
    InvalidScanOrderEntry(usize),
    /// The provided scan-order table's length does not match the
    /// destination's capacity. The two integers are `(scan_order_len,
    /// destination_len)`.
    ScanOrderLengthMismatch(usize, usize),
}

impl core::fmt::Display for ScanError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ScanError::OutOfRange(cursor) => {
                write!(
                    f,
                    "oxideav-svq: SVQ3 coefficient-placement cursor {cursor} \
                     exceeds destination block capacity"
                )
            }
            ScanError::InvalidScanOrderEntry(entry) => {
                write!(
                    f,
                    "oxideav-svq: SVQ3 scan-order table contains out-of-range \
                     entry {entry}"
                )
            }
            ScanError::ScanOrderLengthMismatch(scan_len, dest_len) => {
                write!(
                    f,
                    "oxideav-svq: SVQ3 scan-order table length {scan_len} \
                     does not match destination capacity {dest_len}"
                )
            }
        }
    }
}

impl std::error::Error for ScanError {}

/// Place a `(run, value)` coefficient stream into a destination block
/// according to a scan-order table.
///
/// The placement walks `coeffs` in stream order. Per the wiki spec's
/// §"Coefficient decoding" the cursor advances `coeff.run + 1`
/// positions per non-zero coefficient: the first `run` zero positions
/// are left untouched and the coefficient's signed `value` is written
/// at the (cursor + run) scan position.
///
/// `scan_order` maps each in-scan position to a flat-index in the
/// destination. For the 2×2 chroma DC case use [`CHROMA_DC_2X2_SCAN`].
///
/// The destination is initialised to all zeros and only the cursor's
/// scan positions receive coefficient values; positions skipped by a
/// non-zero `run` keep their zero initialisation. Returns the populated
/// destination on success.
///
/// Errors:
///
/// * [`ScanError::OutOfRange`] — a `(run + 1)` advance would push the
///   cursor past `scan_order.len()`.
/// * [`ScanError::InvalidScanOrderEntry`] — a `scan_order` entry is
///   itself past the destination's flat capacity. This signals a
///   malformed scan-order table, not a malformed input stream.
/// * [`ScanError::ScanOrderLengthMismatch`] — the `scan_order` slice's
///   length does not match the destination's flat capacity `DEST_LEN`.
pub fn place_coefficients_in_scan_order<const DEST_LEN: usize>(
    coeffs: &[Coefficient],
    scan_order: &[usize],
) -> Result<[i32; DEST_LEN], ScanError> {
    if scan_order.len() != DEST_LEN {
        return Err(ScanError::ScanOrderLengthMismatch(
            scan_order.len(),
            DEST_LEN,
        ));
    }
    // Validate the scan-order table once up front so the placement
    // loop can index without re-checking.
    for &entry in scan_order {
        if entry >= DEST_LEN {
            return Err(ScanError::InvalidScanOrderEntry(entry));
        }
    }
    let mut dest = [0i32; DEST_LEN];
    let mut cursor: usize = 0;
    for coeff in coeffs {
        let advance = (coeff.run as usize)
            .checked_add(1)
            .ok_or(ScanError::OutOfRange(usize::MAX))?;
        let place_at = cursor
            .checked_add(coeff.run as usize)
            .ok_or(ScanError::OutOfRange(usize::MAX))?;
        if place_at >= scan_order.len() {
            return Err(ScanError::OutOfRange(place_at));
        }
        let flat_index = scan_order[place_at];
        // Scan-order entries pre-validated above; bounds are sound.
        dest[flat_index] = coeff.value;
        cursor = cursor
            .checked_add(advance)
            .ok_or(ScanError::OutOfRange(usize::MAX))?;
        if cursor > scan_order.len() {
            return Err(ScanError::OutOfRange(cursor));
        }
    }
    Ok(dest)
}

/// Place a chroma DC 2×2 coefficient stream into a 4-entry flat block.
///
/// Convenience wrapper around [`place_coefficients_in_scan_order`]
/// pinned to [`CHROMA_DC_2X2_SCAN`] and [`CHROMA_DC_2X2_LEN`]. Returns
/// the 4-entry block in row-major order (so `block[0]` is `(0,0)`,
/// `block[1]` is `(0,1)`, `block[2]` is `(1,0)`, `block[3]` is
/// `(1,1)`), suitable for feeding directly into the
/// [`crate::svq3_dequant::CHROMA_DC_TRANSFORM_MATRIX`] application.
pub fn place_chroma_dc_2x2(coeffs: &[Coefficient]) -> Result<[i32; CHROMA_DC_2X2_LEN], ScanError> {
    place_coefficients_in_scan_order::<CHROMA_DC_2X2_LEN>(coeffs, &CHROMA_DC_2X2_SCAN)
}

/// Place a 4×4 coefficient stream into a 16-entry flat block using the
/// **normal-zigzag** scan ([`NORMAL_ZIGZAG_4X4_SCAN`]).
///
/// Convenience wrapper around [`place_coefficients_in_scan_order`]. The
/// returned block is in 4×4 row-major order (so `block[r * 4 + c]` is
/// the coefficient at matrix position `(r, c)`), ready for the
/// [`crate::svq3_dequant`] per-coefficient dequant.
pub fn place_4x4_normal_zigzag(coeffs: &[Coefficient]) -> Result<[i32; FULL_4X4_LEN], ScanError> {
    place_coefficients_in_scan_order::<FULL_4X4_LEN>(coeffs, &NORMAL_ZIGZAG_4X4_SCAN)
}

/// Place a 4×4 coefficient stream into a 16-entry flat block using the
/// **alternate scan** ([`ALT_SCAN_4X4_SCAN`]).
///
/// Convenience wrapper around [`place_coefficients_in_scan_order`]. The
/// returned block is in 4×4 row-major order. Per spec/01 Gap 1 this scan
/// is only correct for a luma block of a 4×4-intra MB at quantiser
/// `< 24`; use [`select_4x4_scan`] / [`place_4x4`] to apply the
/// selection rule.
pub fn place_4x4_alt_scan(coeffs: &[Coefficient]) -> Result<[i32; FULL_4X4_LEN], ScanError> {
    place_coefficients_in_scan_order::<FULL_4X4_LEN>(coeffs, &ALT_SCAN_4X4_SCAN)
}

/// Place a 4×4 coefficient stream, selecting the scan order per the
/// spec/01 Gap 1 rule.
///
/// Picks [`ALT_SCAN_4X4_SCAN`] when `is_luma_4x4_intra && quantiser < 24`
/// (see [`select_4x4_scan`]) and [`NORMAL_ZIGZAG_4X4_SCAN`] otherwise,
/// then walks the `(run, value)` stream into a 16-entry 4×4 row-major
/// block.
pub fn place_4x4(
    coeffs: &[Coefficient],
    is_luma_4x4_intra: bool,
    quantiser: u32,
) -> Result<[i32; FULL_4X4_LEN], ScanError> {
    let scan = select_4x4_scan(is_luma_4x4_intra, quantiser);
    place_coefficients_in_scan_order::<FULL_4X4_LEN>(coeffs, scan)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 2×2 row-major scan-order invariants ---------------------------

    #[test]
    fn chroma_dc_2x2_scan_is_identity_row_major() {
        // For a 2×2 block stored row-major, the row-major scan order
        // is the identity mapping `[0, 1, 2, 3]` — the flat index at
        // each in-scan position equals the in-scan position itself.
        assert_eq!(CHROMA_DC_2X2_SCAN, [0, 1, 2, 3]);
    }

    #[test]
    fn chroma_dc_2x2_scan_has_4_entries() {
        assert_eq!(CHROMA_DC_2X2_SCAN.len(), CHROMA_DC_2X2_LEN);
        assert_eq!(CHROMA_DC_2X2_LEN, 4);
    }

    #[test]
    fn chroma_dc_2x2_scan_covers_every_position_exactly_once() {
        let mut seen = [false; CHROMA_DC_2X2_LEN];
        for &entry in &CHROMA_DC_2X2_SCAN {
            assert!(entry < CHROMA_DC_2X2_LEN, "entry {entry} out of range");
            assert!(!seen[entry], "entry {entry} seen twice");
            seen[entry] = true;
        }
        assert!(seen.iter().all(|&b| b), "scan order missed a position");
    }

    #[test]
    fn chroma_dc_2x2_scan_entries_decode_to_expected_matrix_positions() {
        // For row-major storage, scan position 0 → matrix (0,0),
        // 1 → (0,1), 2 → (1,0), 3 → (1,1).
        let expected_positions = [(0usize, 0usize), (0, 1), (1, 0), (1, 1)];
        for (i, &expected) in expected_positions.iter().enumerate() {
            let pos =
                chroma_dc_2x2_matrix_position(CHROMA_DC_2X2_SCAN[i]).expect("flat index in range");
            assert_eq!(pos, expected, "scan position {i}");
        }
    }

    // ---- Flat-index helpers --------------------------------------------

    #[test]
    fn chroma_dc_2x2_flat_index_round_trips() {
        for row in 0..2 {
            for col in 0..2 {
                let flat = chroma_dc_2x2_flat_index(row, col).expect("in range");
                assert!(flat < CHROMA_DC_2X2_LEN);
                let back = chroma_dc_2x2_matrix_position(flat).expect("in range");
                assert_eq!(back, (row, col));
            }
        }
    }

    #[test]
    fn chroma_dc_2x2_flat_index_rejects_out_of_range_row() {
        assert!(chroma_dc_2x2_flat_index(2, 0).is_none());
        assert!(chroma_dc_2x2_flat_index(usize::MAX, 0).is_none());
    }

    #[test]
    fn chroma_dc_2x2_flat_index_rejects_out_of_range_col() {
        assert!(chroma_dc_2x2_flat_index(0, 2).is_none());
        assert!(chroma_dc_2x2_flat_index(0, usize::MAX).is_none());
    }

    #[test]
    fn chroma_dc_2x2_matrix_position_rejects_out_of_range_flat_index() {
        assert!(chroma_dc_2x2_matrix_position(4).is_none());
        assert!(chroma_dc_2x2_matrix_position(usize::MAX).is_none());
    }

    // ---- Placement: empty stream ---------------------------------------

    #[test]
    fn empty_stream_yields_all_zero_block() {
        let block = place_chroma_dc_2x2(&[]).unwrap();
        assert_eq!(block, [0, 0, 0, 0]);
    }

    // ---- Placement: single coefficient at each position ---------------

    #[test]
    fn single_coefficient_at_position_0_writes_value_at_index_0() {
        // run=0 → cursor advance 1, place at scan position 0 = flat 0.
        let block = place_chroma_dc_2x2(&[Coefficient { run: 0, value: 7 }]).unwrap();
        assert_eq!(block, [7, 0, 0, 0]);
    }

    #[test]
    fn single_coefficient_with_run_1_writes_value_at_scan_position_1() {
        // run=1 → place at scan position 0+1 = 1; flat = scan_order[1] = 1.
        let block = place_chroma_dc_2x2(&[Coefficient { run: 1, value: 5 }]).unwrap();
        assert_eq!(block, [0, 5, 0, 0]);
    }

    #[test]
    fn single_coefficient_with_run_3_writes_value_at_scan_position_3() {
        // run=3 → place at scan position 0+3 = 3; flat = scan_order[3] = 3.
        let block = place_chroma_dc_2x2(&[Coefficient { run: 3, value: -2 }]).unwrap();
        assert_eq!(block, [0, 0, 0, -2]);
    }

    // ---- Placement: multi-coefficient streams --------------------------

    #[test]
    fn two_coefficients_both_run_zero_fill_consecutive_positions() {
        let block = place_chroma_dc_2x2(&[
            Coefficient { run: 0, value: 1 },
            Coefficient { run: 0, value: 2 },
        ])
        .unwrap();
        assert_eq!(block, [1, 2, 0, 0]);
    }

    #[test]
    fn run_between_coefficients_skips_zero_positions() {
        // First coeff: run=0, value=1 → place at position 0, cursor → 1.
        // Second coeff: run=1, value=4 → place at position 1+1=2, cursor → 3.
        let block = place_chroma_dc_2x2(&[
            Coefficient { run: 0, value: 1 },
            Coefficient { run: 1, value: 4 },
        ])
        .unwrap();
        assert_eq!(block, [1, 0, 4, 0]);
    }

    #[test]
    fn four_coefficients_fill_block_exactly() {
        // Each run=0, value=k → position k.
        let block = place_chroma_dc_2x2(&[
            Coefficient { run: 0, value: 1 },
            Coefficient { run: 0, value: 2 },
            Coefficient { run: 0, value: 3 },
            Coefficient { run: 0, value: 4 },
        ])
        .unwrap();
        assert_eq!(block, [1, 2, 3, 4]);
    }

    #[test]
    fn negative_values_are_preserved_through_placement() {
        let block = place_chroma_dc_2x2(&[
            Coefficient { run: 0, value: -7 },
            Coefficient { run: 1, value: -3 },
        ])
        .unwrap();
        assert_eq!(block, [-7, 0, -3, 0]);
    }

    // ---- Placement: cursor overrun -------------------------------------

    #[test]
    fn run_pushing_cursor_past_capacity_errors() {
        // Block capacity is 4. run=4 would place at scan position 4
        // (out of range) → error.
        let err = place_chroma_dc_2x2(&[Coefficient { run: 4, value: 1 }]).unwrap_err();
        assert!(matches!(err, ScanError::OutOfRange(_)));
    }

    #[test]
    fn cumulative_runs_pushing_cursor_past_capacity_errors() {
        // First coeff places at 0, cursor → 1.
        // Second coeff run=2, place at 1+2=3, cursor → 4.
        // Third coeff would start at cursor=4, even run=0 overflows.
        let err = place_chroma_dc_2x2(&[
            Coefficient { run: 0, value: 1 },
            Coefficient { run: 2, value: 2 },
            Coefficient { run: 0, value: 3 },
        ])
        .unwrap_err();
        assert!(matches!(err, ScanError::OutOfRange(_)));
    }

    #[test]
    fn cursor_overflow_is_caught_for_u32_run_saturated() {
        // run = u32::MAX → run as usize on 64-bit platforms is
        // 0xFFFF_FFFF; adding 1 stays in usize range, but the place_at
        // check fires because the cursor would land past scan_order.len().
        let err = place_chroma_dc_2x2(&[Coefficient {
            run: u32::MAX,
            value: 1,
        }])
        .unwrap_err();
        assert!(matches!(err, ScanError::OutOfRange(_)));
    }

    // ---- Placement: scan-order mismatch errors ------------------------

    #[test]
    fn mismatched_scan_order_length_errors() {
        // A 3-entry scan-order with a 4-entry destination triggers the
        // length-mismatch guard.
        let scan_order: [usize; 3] = [0, 1, 2];
        let err =
            place_coefficients_in_scan_order::<4>(&[Coefficient { run: 0, value: 1 }], &scan_order)
                .unwrap_err();
        assert!(matches!(err, ScanError::ScanOrderLengthMismatch(3, 4)));
    }

    #[test]
    fn out_of_range_scan_order_entry_errors() {
        // 4-entry scan-order whose last entry points past the
        // destination capacity → the up-front validation rejects it
        // before any coefficient is placed.
        let scan_order: [usize; 4] = [0, 1, 2, 99];
        let err =
            place_coefficients_in_scan_order::<4>(&[Coefficient { run: 0, value: 1 }], &scan_order)
                .unwrap_err();
        assert!(matches!(err, ScanError::InvalidScanOrderEntry(99)));
    }

    // ---- Generic placement against an arbitrary permutation -----------

    #[test]
    fn generic_placement_respects_permuted_scan_order() {
        // A 4-entry permuted scan order: scan positions map to flat
        // indices [3, 1, 2, 0] (i.e. scan pos 0 → flat 3, scan pos 1 →
        // flat 1, etc.). Two coeffs at scan positions 0 and 2 (both
        // run=0/1) end up at flat indices 3 and 2 respectively.
        let scan_order: [usize; 4] = [3, 1, 2, 0];
        // Coefficients: (run=0, value=10) and (run=1, value=20).
        let coeffs = [
            Coefficient { run: 0, value: 10 },
            Coefficient { run: 1, value: 20 },
        ];
        let block = place_coefficients_in_scan_order::<4>(&coeffs, &scan_order).unwrap();
        // Scan pos 0 → flat 3 ← 10. Scan pos 0+1+1=2 → flat 2 ← 20.
        assert_eq!(block, [0, 0, 20, 10]);
    }

    #[test]
    fn generic_placement_empty_stream_yields_zeros_for_any_scan_order() {
        let scan_order: [usize; 4] = [2, 3, 0, 1];
        let block = place_coefficients_in_scan_order::<4>(&[], &scan_order).unwrap();
        assert_eq!(block, [0; 4]);
    }

    // ---- Error Display sanity -----------------------------------------

    // ---- 4×4 scan-order arrays (spec/01 Gap 1) ------------------------

    #[test]
    fn normal_zigzag_4x4_matches_spec_bytes() {
        // docs/video/svq3/spec/01-reconstruction-composition.md Gap 1,
        // file offset 0x7e5a8.
        assert_eq!(
            NORMAL_ZIGZAG_4X4_SCAN,
            [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15]
        );
    }

    #[test]
    fn alt_scan_4x4_matches_spec_bytes() {
        // docs/video/svq3/spec/01-reconstruction-composition.md Gap 1,
        // file offset 0x7e5b8.
        assert_eq!(
            ALT_SCAN_4X4_SCAN,
            [0, 1, 2, 6, 10, 3, 7, 11, 4, 8, 5, 9, 12, 13, 14, 15]
        );
    }

    #[test]
    fn both_4x4_scans_are_full_permutations() {
        for scan in [&NORMAL_ZIGZAG_4X4_SCAN, &ALT_SCAN_4X4_SCAN] {
            assert_eq!(scan.len(), FULL_4X4_LEN);
            let mut seen = [false; FULL_4X4_LEN];
            for &entry in scan.iter() {
                assert!(entry < FULL_4X4_LEN, "entry {entry} out of range");
                assert!(!seen[entry], "entry {entry} seen twice");
                seen[entry] = true;
            }
            assert!(seen.iter().all(|&b| b), "scan missed a position");
        }
    }

    #[test]
    fn both_4x4_scans_start_at_dc() {
        // The DC coefficient (raster index 0) must be the first decoded
        // position in either scan.
        assert_eq!(NORMAL_ZIGZAG_4X4_SCAN[0], 0);
        assert_eq!(ALT_SCAN_4X4_SCAN[0], 0);
    }

    #[test]
    fn alt_scan_splits_into_two_recognised_halves() {
        // Per the wiki §"Macroblock layer" two-half structure, the
        // alt-scan is coded in two parts of up to eight coefficients.
        assert_eq!(ALT_SCAN_4X4_HALF_LEN, 8);
        let (first, second) = ALT_SCAN_4X4_SCAN.split_at(ALT_SCAN_4X4_HALF_LEN);
        assert_eq!(first, [0, 1, 2, 6, 10, 3, 7, 11]);
        assert_eq!(second, [4, 8, 5, 9, 12, 13, 14, 15]);
    }

    #[test]
    fn the_two_4x4_scans_differ() {
        // Confirms we transcribed two distinct tables, not the same one.
        assert_ne!(NORMAL_ZIGZAG_4X4_SCAN, ALT_SCAN_4X4_SCAN);
    }

    // ---- 4×4 scan selection rule (spec/01 Gap 1) ----------------------

    #[test]
    fn select_4x4_uses_alt_scan_only_for_luma_4x4_intra_below_threshold() {
        assert_eq!(ALT_SCAN_QUANTISER_THRESHOLD, 24);
        // luma 4×4-intra, Q < 24 → alt-scan.
        assert_eq!(select_4x4_scan(true, 0), &ALT_SCAN_4X4_SCAN);
        assert_eq!(select_4x4_scan(true, 23), &ALT_SCAN_4X4_SCAN);
        // boundary: Q == 24 → normal zigzag.
        assert_eq!(select_4x4_scan(true, 24), &NORMAL_ZIGZAG_4X4_SCAN);
        // Q > 24 → normal zigzag.
        assert_eq!(select_4x4_scan(true, 31), &NORMAL_ZIGZAG_4X4_SCAN);
        // not luma-4×4-intra (e.g. inter / 16×16-intra / chroma AC) →
        // normal zigzag regardless of quantiser.
        assert_eq!(select_4x4_scan(false, 0), &NORMAL_ZIGZAG_4X4_SCAN);
        assert_eq!(select_4x4_scan(false, 23), &NORMAL_ZIGZAG_4X4_SCAN);
    }

    // ---- 4×4 placement wrappers ---------------------------------------

    #[test]
    fn place_4x4_normal_zigzag_places_dc_then_first_ac() {
        // Two run=0 coeffs land at scan positions 0 and 1 → raster
        // indices NORMAL_ZIGZAG[0]=0 and NORMAL_ZIGZAG[1]=1.
        let block = place_4x4_normal_zigzag(&[
            Coefficient { run: 0, value: 9 },
            Coefficient { run: 0, value: 4 },
        ])
        .unwrap();
        let mut expected = [0i32; FULL_4X4_LEN];
        expected[0] = 9;
        expected[1] = 4;
        assert_eq!(block, expected);
    }

    #[test]
    fn place_4x4_alt_scan_routes_third_coefficient_to_raster_index_2() {
        // Scan positions 0,1,2 in alt-scan map to raster 0,1,2.
        // A run=2 on the first coeff places it at scan position 2 →
        // ALT_SCAN[2] = 2.
        let block = place_4x4_alt_scan(&[Coefficient { run: 2, value: 5 }]).unwrap();
        let mut expected = [0i32; FULL_4X4_LEN];
        expected[ALT_SCAN_4X4_SCAN[2]] = 5;
        assert_eq!(block, expected);
        assert_eq!(block[2], 5);
    }

    #[test]
    fn place_4x4_dispatches_on_selection_rule() {
        let coeffs = [Coefficient { run: 5, value: 11 }];
        // luma 4×4-intra, Q=10 → alt-scan, scan pos 5 → ALT_SCAN[5]=3.
        let alt = place_4x4(&coeffs, true, 10).unwrap();
        assert_eq!(alt[ALT_SCAN_4X4_SCAN[5]], 11);
        assert_eq!(alt, place_4x4_alt_scan(&coeffs).unwrap());
        // same block at Q=24 → normal zigzag, scan pos 5 → ZIGZAG[5]=2.
        let zig = place_4x4(&coeffs, true, 24).unwrap();
        assert_eq!(zig[NORMAL_ZIGZAG_4X4_SCAN[5]], 11);
        assert_eq!(zig, place_4x4_normal_zigzag(&coeffs).unwrap());
        // the two routings differ for this coefficient.
        assert_ne!(alt, zig);
    }

    #[test]
    fn place_4x4_full_block_normal_zigzag_is_scan_permutation() {
        // Sixteen run=0 coeffs with value = scan-position fill every
        // raster cell with its inverse-permuted scan position.
        let coeffs: Vec<Coefficient> = (0..FULL_4X4_LEN as i32)
            .map(|v| Coefficient {
                run: 0,
                value: v + 1,
            })
            .collect();
        let block = place_4x4_normal_zigzag(&coeffs).unwrap();
        for (scan_pos, &raster) in NORMAL_ZIGZAG_4X4_SCAN.iter().enumerate() {
            assert_eq!(block[raster], scan_pos as i32 + 1);
        }
    }

    #[test]
    fn scan_error_display_messages_mention_module_prefix() {
        let msg = format!("{}", ScanError::OutOfRange(99));
        assert!(msg.contains("oxideav-svq"), "got {msg}");
        assert!(msg.contains("99"), "got {msg}");
        let msg = format!("{}", ScanError::InvalidScanOrderEntry(7));
        assert!(msg.contains("oxideav-svq"), "got {msg}");
        let msg = format!("{}", ScanError::ScanOrderLengthMismatch(3, 4));
        assert!(msg.contains("oxideav-svq"), "got {msg}");
        assert!(msg.contains('3'), "got {msg}");
        assert!(msg.contains('4'), "got {msg}");
    }
}
