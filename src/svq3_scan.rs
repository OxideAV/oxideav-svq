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
//! * **4×4 alt-scan ("Dezigzag pattern (from H.264)" picture)** — the
//!   wiki spec depicts a 16-position scan order in §"Macroblock layer"
//!   as ASCII art. The picture has two recognised-ambiguous
//!   characteristics (see "Open work" below) so round 233 does **not**
//!   transcribe the 4×4 scan-order array. The infrastructure
//!   [`place_coefficients_in_scan_order`] can consume it once the array
//!   is pinned by a future docs round.
//!
//! * **4×4 normal-zigzag (default case)** — the wiki spec mentions
//!   "normal zigzag is used" in §"Macroblock layer" but does not depict
//!   a scan-order picture for this case. Round 233 does **not**
//!   transcribe a 4×4 normal-zigzag array.
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
//! * The wiki's §"Macroblock layer" "Dezigzag pattern (from H.264)"
//!   ASCII art has two unresolved characteristics: (a) the picture's
//!   row-0 horizontal arrows connect three adjacent positions
//!   `(0,0)→(0,1)→(0,2)`, which is not the H.264 frame-zigzag opening
//!   triple `(0,0)→(0,1)→(1,0)` and is also not the H.264 alt-scan
//!   opening triple `(0,0)→(1,0)→(0,1)`; (b) the wiki text uses "normal
//!   zigzag" as the not-this-picture case without depicting a second
//!   pattern. Round 233 surfaces the placement infrastructure and the
//!   unambiguous chroma DC 2×2 scan; the 4×4 scan-order arrays for
//!   both alt-scan and normal-zigzag cases are deferred to the round
//!   that pins their canonical interpretation in `docs/video/svq3/`.
//!
//! * Round 233 does NOT wire the placement output into
//!   [`crate::svq3_dequant`] — the dezigzag step's caller will perform
//!   that wiring once the 4×4 scan-order arrays land.

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
