//! SVQ3 residual coefficient walker.
//!
//! Implements the per-block Golomb-coded `(run, value)` residual
//! coefficient stream described in
//! `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient decoding".
//! Round 5 lands the three coefficient-table variants the wiki spec
//! enumerates plus the two run-correction arrays:
//!
//! * **Chroma DC** — the 2×2 chroma DC block (4 coefficients), one of
//!   three closed-form lookups: codes `0..=2` → `(run = 0, value =
//!   code)`, code `3` → `(run = 1, value = 1)`, codes `4..` →
//!   `(run = code & 0x3, value = ((code + 9) >> 2) - run)`.
//! * **Alternative scan** — used for luma 4×4 intra MBs when the
//!   quantiser is `< 24` (per the wiki spec's §"Macroblock layer"
//!   trailer). A 16-entry explicit table for codes `0..=15` plus a
//!   closed-form extension for codes `>= 16` keyed by the
//!   [`INTRA_RUN_CORRECTION`] array.
//! * **All other blocks** — the normal-zigzag block table. A 16-entry
//!   explicit table for codes `0..=15` plus a closed-form extension
//!   keyed by the [`INTER_RUN_CORRECTION`] array.
//!
//! The wiki spec also documents the per-coefficient sign bit ("last bit
//! is coefficient sign") and the end-of-block sentinel ("code = 0 means
//! end of nonzero coefficients"). Both are implemented by the
//! [`read_normal_scan_block`] / [`read_alt_scan_half`] /
//! [`read_chroma_dc_block`] block-level walkers exposed here.
//!
//! ## What this module does **not** do
//!
//! * It does not apply de-zigzag. The wiki spec's §"Macroblock layer"
//!   describes the de-zigzag pattern but the actual coefficient → 4×4
//!   transform-coefficient-matrix placement happens in a later round
//!   that wires the walker output to the IDCT input.
//! * It does not dequantise. The wiki spec's §"Macroblock transform
//!   and dequantization" gives the `svq3_dequant_coeff[Q]` table and
//!   the dequant formula but those land in a later round alongside the
//!   IDCT.
//! * It does not consume the half-scan boundary. The wiki spec notes
//!   the alt-scan block "is coded in two parts of up two eight
//!   coefficients corresponding to each half-scan"; the walker is
//!   invoked **once per half-scan** by the caller, with a max
//!   coefficient count of `8`.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::svq3::read_ue_golomb;

/// Run-correction table used by the alternative-scan coefficient walker.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient
/// decoding": `intra_run = { 8, 2, 0, 0, 0, -1, -1, -1, [minus ones] }`.
///
/// The wiki spec uses the label `intra_run` for the table that pairs
/// with the alternative-scan codes (the table itself documented in the
/// "Codes for blocks using an alternative scan" wiki section); the
/// label is preserved here. Indices in `0..=4` carry positive
/// corrections; indices `>= 5` collapse to the constant `-1` per the
/// `[minus ones]` shorthand at the tail of the spec array.
///
/// Used by the extension-code formula
/// `value = (code >> 3) - INTRA_RUN_CORRECTION[run]` when
/// `code >= 16` in the alternative-scan table.
pub const INTRA_RUN_CORRECTION: [i32; 8] = [8, 2, 0, 0, 0, -1, -1, -1];

/// Run-correction table used by the normal-scan ("all other blocks")
/// coefficient walker.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient
/// decoding": `inter_run = { 4, 2, 2, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0,
/// 0, 0, 0, [zeroes] }`.
///
/// Indices in `0..=16` carry the explicit values from the spec;
/// indices `>= 17` collapse to `0` per the `[zeroes]` shorthand at the
/// tail of the spec array.
///
/// Used by the extension-code formula
/// `value = (code >> 4) - INTER_RUN_CORRECTION[run]` when `code >= 16`
/// in the normal-scan table.
pub const INTER_RUN_CORRECTION: [i32; 17] = [4, 2, 2, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];

/// 16-entry explicit `(run, value)` lookup for the alternative-scan
/// coefficient table when the Golomb-decoded `code` is in `0..=15`.
///
/// Verbatim transcription of the wiki spec's
/// "Codes for blocks using an alternative scan" table, codes 0 through
/// 15.
pub const ALT_SCAN_TABLE_0_15: [(u32, i32); 16] = [
    (0, 0),
    (0, 1),
    (1, 1),
    (0, 2),
    (2, 1),
    (0, 3),
    (0, 4),
    (0, 5),
    (3, 1),
    (4, 1),
    (1, 2),
    (1, 3),
    (0, 6),
    (0, 7),
    (0, 8),
    (0, 9),
];

/// 16-entry explicit `(run, value)` lookup for the normal-scan
/// coefficient table when the Golomb-decoded `code` is in `0..=15`.
///
/// Verbatim transcription of the wiki spec's
/// "Codes for all other block types" table, codes 0 through 15.
pub const NORMAL_SCAN_TABLE_0_15: [(u32, i32); 16] = [
    (0, 0),
    (0, 1),
    (1, 1),
    (2, 1),
    (0, 2),
    (3, 1),
    (4, 1),
    (5, 1),
    (0, 3),
    (1, 2),
    (2, 2),
    (6, 1),
    (7, 1),
    (8, 1),
    (9, 1),
    (0, 4),
];

/// Maximum coefficient count for a 4×4 block (luma / chroma AC).
/// Used as the upper bound for the normal-scan / alt-scan
/// walkers' run accumulator so a malformed stream cannot blow past
/// the block boundary.
pub const COEFFS_PER_4X4_BLOCK: usize = 16;

/// Maximum coefficient count for a 2×2 chroma DC block.
pub const COEFFS_PER_CHROMA_DC_BLOCK: usize = 4;

/// Maximum coefficient count per half-scan when the alternative-scan
/// path is used. The wiki spec's §"Coefficient decoding" notes the
/// alt-scan block is "coded in two parts of up two eight coefficients
/// corresponding to each half-scan".
pub const COEFFS_PER_ALT_SCAN_HALF: usize = 8;

/// A single decoded `(run, signed_value)` coefficient triple.
///
/// `run` is the number of zero coefficients **preceding** this entry
/// (per the wiki spec table's `run` column). `value` is the
/// already-signed coefficient magnitude (the wiki spec's positive table
/// value combined with the per-coefficient sign bit read after the
/// Golomb code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coefficient {
    /// Number of zero coefficients preceding this entry in the scan
    /// order. The block walker's caller is responsible for placing
    /// the value at scan-position `cursor + run` and advancing the
    /// cursor by `run + 1`.
    pub run: u32,
    /// Signed coefficient magnitude. Combines the wiki spec table's
    /// positive `value` field with the sign bit read after the Golomb
    /// code.
    pub value: i32,
}

/// Decode one residual coefficient from the 2×2 chroma DC table.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient
/// decoding" the 2×2 chroma DC block uses the lookup:
///
/// | `code` | `run` | `value`                       |
/// | ------ | ----- | ------------------------------ |
/// | `0..=2` | `0`   | `code`                        |
/// | `3`     | `1`   | `1`                           |
/// | `4..`   | `code & 0x3` | `((code + 9) >> 2) - run` |
///
/// Returns:
///
/// * `Ok(None)` — Golomb code decoded to `0`, signalling end of block
///   (per the wiki spec's "code = 0 means end of nonzero
///   coefficients"). No sign bit is consumed in that case.
/// * `Ok(Some(coeff))` — a non-zero coefficient. The sign bit that
///   immediately follows the Golomb code in the bit stream is consumed
///   and used to negate the table's positive `value` when set.
/// * `Err(Error::Truncated)` — the bit-reader ran out of bits before
///   the Golomb code or the trailing sign bit could be read.
pub fn read_chroma_dc_coefficient(br: &mut BitReader<'_>) -> Result<Option<Coefficient>> {
    let code = read_ue_golomb(br)?;
    if code == 0 {
        return Ok(None);
    }
    let (run, value) = match code {
        // Codes 1..=2 fall in the "0-2" row of the spec table:
        // (run = 0, value = code). Code 0 is the end-of-block
        // sentinel handled above.
        1 | 2 => (0u32, code as i32),
        3 => (1u32, 1i32),
        other => {
            let run = other & 0x3;
            let value = ((other + 9) >> 2) as i32 - run as i32;
            (run, value)
        }
    };
    let sign = br.read_bit()?;
    let signed_value = if sign == 1 { -value } else { value };
    Ok(Some(Coefficient {
        run,
        value: signed_value,
    }))
}

/// Decode one residual coefficient from the alternative-scan table.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient
/// decoding" — used for luma 4×4 intra blocks when the quantiser is
/// `< 24`. The lookup combines the 16-entry [`ALT_SCAN_TABLE_0_15`]
/// for codes `0..=15` with the closed-form extension
/// `(run = code & 0x7, value = (code >> 3) - INTRA_RUN_CORRECTION[run])`
/// for codes `>= 16`.
///
/// Returns the same end-of-block / non-zero / truncated signalling as
/// [`read_chroma_dc_coefficient`].
pub fn read_alt_scan_coefficient(br: &mut BitReader<'_>) -> Result<Option<Coefficient>> {
    let code = read_ue_golomb(br)?;
    if code == 0 {
        return Ok(None);
    }
    let (run, value) = if (code as usize) < ALT_SCAN_TABLE_0_15.len() {
        ALT_SCAN_TABLE_0_15[code as usize]
    } else {
        let run = code & 0x7;
        let correction = INTRA_RUN_CORRECTION
            .get(run as usize)
            .copied()
            // The wiki spec describes the array's tail as
            // `[minus ones]`, so out-of-table indices collapse to -1.
            .unwrap_or(-1);
        let value = (code >> 3) as i32 - correction;
        (run, value)
    };
    let sign = br.read_bit()?;
    let signed_value = if sign == 1 { -value } else { value };
    Ok(Some(Coefficient {
        run,
        value: signed_value,
    }))
}

/// Decode one residual coefficient from the normal-scan ("all other
/// block types") table.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient
/// decoding". Combines the 16-entry [`NORMAL_SCAN_TABLE_0_15`] for
/// codes `0..=15` with the closed-form extension
/// `(run = code & 0xF, value = (code >> 4) - INTER_RUN_CORRECTION[run])`
/// for codes `>= 16`.
///
/// Returns the same end-of-block / non-zero / truncated signalling as
/// [`read_chroma_dc_coefficient`].
pub fn read_normal_scan_coefficient(br: &mut BitReader<'_>) -> Result<Option<Coefficient>> {
    let code = read_ue_golomb(br)?;
    if code == 0 {
        return Ok(None);
    }
    let (run, value) = if (code as usize) < NORMAL_SCAN_TABLE_0_15.len() {
        NORMAL_SCAN_TABLE_0_15[code as usize]
    } else {
        let run = code & 0xF;
        let correction = INTER_RUN_CORRECTION
            .get(run as usize)
            .copied()
            // The wiki spec describes the array's tail as `[zeroes]`,
            // so out-of-table indices collapse to 0.
            .unwrap_or(0);
        let value = (code >> 4) as i32 - correction;
        (run, value)
    };
    let sign = br.read_bit()?;
    let signed_value = if sign == 1 { -value } else { value };
    Ok(Some(Coefficient {
        run,
        value: signed_value,
    }))
}

/// Walk the chroma DC block (2×2 = 4 coefficients) at the bit-reader
/// cursor.
///
/// Reads coefficients via [`read_chroma_dc_coefficient`] until either
/// the end-of-block sentinel (`code == 0`) is seen or the accumulated
/// scan position reaches [`COEFFS_PER_CHROMA_DC_BLOCK`].
///
/// Returns the decoded `(run, signed_value)` triples in stream order.
/// Errors:
///
/// * [`Error::Truncated`] — bit-reader ran out mid-coefficient.
/// * [`Error::BadBitWidth`] — used as the structural-overflow signal
///   when a decoded `(run, value)` would push the scan cursor past
///   [`COEFFS_PER_CHROMA_DC_BLOCK`]. The argument is the would-be
///   scan position so the caller can include it in diagnostics.
pub fn read_chroma_dc_block(br: &mut BitReader<'_>) -> Result<Vec<Coefficient>> {
    read_block_inner(br, COEFFS_PER_CHROMA_DC_BLOCK, read_chroma_dc_coefficient)
}

/// Walk one half-scan of an alternative-scan block (8 coefficients).
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient
/// decoding": "in this case block is coded in two parts of up two
/// eight coefficients corresponding to each half-scan". The caller is
/// responsible for invoking this function **twice** per alt-scan block
/// (once per half) and gathering both halves into the 16-coefficient
/// block.
///
/// Reads coefficients via [`read_alt_scan_coefficient`] until either
/// the end-of-block sentinel is seen or the accumulated scan position
/// reaches [`COEFFS_PER_ALT_SCAN_HALF`].
pub fn read_alt_scan_half(br: &mut BitReader<'_>) -> Result<Vec<Coefficient>> {
    read_block_inner(br, COEFFS_PER_ALT_SCAN_HALF, read_alt_scan_coefficient)
}

/// One alternative-scan 4×4 block, organised into its two half-scans as
/// the wiki spec describes them.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient
/// decoding" line "Please note that in this case block is coded in two
/// parts of up two eight coefficients corresponding to each half-scan."
/// Each half is an independent run of up to [`COEFFS_PER_ALT_SCAN_HALF`]
/// = `8` coefficients, terminated by either an explicit end-of-block
/// sentinel (`code == 0`) or by reaching the half's capacity.
///
/// The two halves are kept **separated**: the run-accumulator cursor
/// resets at the half boundary so a coefficient placed at scan
/// position `7` in the first half does not consume a slot in the second
/// half. This mirrors the wiki spec's "two parts" wording — each part is
/// its own independent run.
///
/// The block's flat order is `first_half ++ second_half` (first-half
/// coefficients come before second-half coefficients in stream order).
/// The cumulative count of `first_half.len() + second_half.len()` is
/// bounded by `2 * COEFFS_PER_ALT_SCAN_HALF = 16` =
/// [`COEFFS_PER_4X4_BLOCK`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltScanBlock {
    /// Coefficients read from the first half-scan (up to
    /// [`COEFFS_PER_ALT_SCAN_HALF`] entries).
    pub first_half: Vec<Coefficient>,
    /// Coefficients read from the second half-scan (up to
    /// [`COEFFS_PER_ALT_SCAN_HALF`] entries).
    pub second_half: Vec<Coefficient>,
}

impl AltScanBlock {
    /// Total non-zero coefficient count across both half-scans.
    ///
    /// Bounded by `2 * COEFFS_PER_ALT_SCAN_HALF = 16` =
    /// [`COEFFS_PER_4X4_BLOCK`].
    pub fn len(&self) -> usize {
        self.first_half.len() + self.second_half.len()
    }

    /// True when both halves are empty (both terminated by an
    /// immediate end-of-block sentinel).
    pub fn is_empty(&self) -> bool {
        self.first_half.is_empty() && self.second_half.is_empty()
    }

    /// Yield a flat `(half_index, coefficient)` iterator across the
    /// block in stream order. `half_index` is `0` for the first-half
    /// coefficients and `1` for the second-half coefficients.
    pub fn iter_with_half(&self) -> impl Iterator<Item = (u8, Coefficient)> + '_ {
        let first = self.first_half.iter().map(|c| (0u8, *c));
        let second = self.second_half.iter().map(|c| (1u8, *c));
        first.chain(second)
    }

    /// Flatten both halves into a single `Vec<Coefficient>` in stream
    /// order (first half, then second half). The half-scan boundary
    /// information is discarded; callers that need it should consume
    /// the [`first_half`](Self::first_half) and
    /// [`second_half`](Self::second_half) fields directly.
    pub fn flatten(&self) -> Vec<Coefficient> {
        let mut out = Vec::with_capacity(self.len());
        out.extend_from_slice(&self.first_half);
        out.extend_from_slice(&self.second_half);
        out
    }
}

/// Walk an alternative-scan 4×4 block as two consecutive half-scans.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient
/// decoding" — the alt-scan path is "coded in two parts of up two eight
/// coefficients corresponding to each half-scan". This helper calls
/// [`read_alt_scan_half`] twice, in order, and packages the result into
/// an [`AltScanBlock`] that preserves the half boundary.
///
/// The two halves are read **independently**: each half has its own
/// run-accumulator cursor that resets to zero at the half boundary, and
/// each half is independently terminated by either an explicit
/// end-of-block sentinel or by reaching `COEFFS_PER_ALT_SCAN_HALF` = `8`
/// coefficients.
///
/// Errors propagate from the underlying half walker:
///
/// * [`Error::Truncated`] — bit-reader ran out of bits mid-coefficient
///   in either half.
/// * [`Error::BadBitWidth`] — a `(run + 1)` advance would push the
///   per-half cursor past `COEFFS_PER_ALT_SCAN_HALF`.
pub fn read_alt_scan_block(br: &mut BitReader<'_>) -> Result<AltScanBlock> {
    let first_half = read_alt_scan_half(br)?;
    let second_half = read_alt_scan_half(br)?;
    Ok(AltScanBlock {
        first_half,
        second_half,
    })
}

/// Walk a 4×4 normal-scan ("all other block types") block (16
/// coefficients).
///
/// Reads coefficients via [`read_normal_scan_coefficient`] until either
/// the end-of-block sentinel is seen or the accumulated scan position
/// reaches [`COEFFS_PER_4X4_BLOCK`].
pub fn read_normal_scan_block(br: &mut BitReader<'_>) -> Result<Vec<Coefficient>> {
    read_block_inner(br, COEFFS_PER_4X4_BLOCK, read_normal_scan_coefficient)
}

/// Shared block-walker body for all three coefficient tables.
///
/// `max_coeffs` is the block's coefficient capacity; `read_one` is one
/// of the per-table single-coefficient readers. The function loops,
/// accumulating decoded coefficients while tracking the implied scan
/// position (`cursor += run + 1` per non-zero coefficient). The loop
/// terminates on either:
///
/// * `read_one` returning `Ok(None)` — end-of-block sentinel.
/// * `cursor` reaching `max_coeffs` without an explicit sentinel — the
///   block is full, the caller is expected to stop reading.
///
/// Returns [`Error::BadBitWidth`] (re-used as the structural-overflow
/// signal) when an in-stream `(run, value)` triple would push the
/// scan cursor past `max_coeffs`.
fn read_block_inner<F>(
    br: &mut BitReader<'_>,
    max_coeffs: usize,
    mut read_one: F,
) -> Result<Vec<Coefficient>>
where
    F: FnMut(&mut BitReader<'_>) -> Result<Option<Coefficient>>,
{
    let mut out: Vec<Coefficient> = Vec::new();
    let mut cursor: usize = 0;
    while cursor < max_coeffs {
        match read_one(br)? {
            None => return Ok(out),
            Some(coeff) => {
                let advance = (coeff.run as usize)
                    .checked_add(1)
                    .ok_or(Error::BadBitWidth(coeff.run))?;
                let next_cursor = cursor
                    .checked_add(advance)
                    .ok_or(Error::BadBitWidth(coeff.run))?;
                if next_cursor > max_coeffs {
                    return Err(Error::BadBitWidth(next_cursor as u32));
                }
                cursor = next_cursor;
                out.push(coeff);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: pack a sequence of `(width, value)` items into a byte
    /// stream by writing them MSB-first. Mirrors the helper used by
    /// the other tests modules in the crate.
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

    /// `ue(n)` exp-Golomb encoding helper — produces a `(width,
    /// value)` pack item that decodes to `n`.
    fn ue(n: u32) -> (u32, u32) {
        let p = n + 1;
        let leading = 31 - p.leading_zeros();
        let width = 2 * leading + 1;
        (width, p)
    }

    // ---- Constants / table-shape invariants ----------------------------

    #[test]
    fn intra_run_correction_matches_wiki_spec() {
        // `intra_run = { 8, 2, 0, 0, 0, -1, -1, -1, [minus ones] }`.
        assert_eq!(INTRA_RUN_CORRECTION, [8, 2, 0, 0, 0, -1, -1, -1]);
    }

    #[test]
    fn inter_run_correction_matches_wiki_spec() {
        // `inter_run = { 4, 2, 2, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0,
        // 0, 0, [zeroes] }`.
        assert_eq!(
            INTER_RUN_CORRECTION,
            [4, 2, 2, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn alt_scan_table_matches_wiki_spec() {
        // Rows 0..=15 from the wiki spec's "Codes for blocks using an
        // alternative scan" table.
        let expected: [(u32, i32); 16] = [
            (0, 0),
            (0, 1),
            (1, 1),
            (0, 2),
            (2, 1),
            (0, 3),
            (0, 4),
            (0, 5),
            (3, 1),
            (4, 1),
            (1, 2),
            (1, 3),
            (0, 6),
            (0, 7),
            (0, 8),
            (0, 9),
        ];
        assert_eq!(ALT_SCAN_TABLE_0_15, expected);
    }

    #[test]
    fn normal_scan_table_matches_wiki_spec() {
        // Rows 0..=15 from the wiki spec's "Codes for all other block
        // types" table.
        let expected: [(u32, i32); 16] = [
            (0, 0),
            (0, 1),
            (1, 1),
            (2, 1),
            (0, 2),
            (3, 1),
            (4, 1),
            (5, 1),
            (0, 3),
            (1, 2),
            (2, 2),
            (6, 1),
            (7, 1),
            (8, 1),
            (9, 1),
            (0, 4),
        ];
        assert_eq!(NORMAL_SCAN_TABLE_0_15, expected);
    }

    #[test]
    fn block_capacity_constants() {
        assert_eq!(COEFFS_PER_4X4_BLOCK, 16);
        assert_eq!(COEFFS_PER_CHROMA_DC_BLOCK, 4);
        assert_eq!(COEFFS_PER_ALT_SCAN_HALF, 8);
    }

    // ---- Chroma DC single-coefficient walker ---------------------------

    #[test]
    fn chroma_dc_code_0_signals_end_of_block() {
        // ue(0) = "1"; no sign bit consumed.
        let bytes = pack(&[ue(0)]);
        let mut br = BitReader::new(&bytes);
        let result = read_chroma_dc_coefficient(&mut br).unwrap();
        assert!(result.is_none());
        assert_eq!(br.bits_consumed(), 1);
    }

    #[test]
    fn chroma_dc_codes_1_and_2_yield_run_0_value_code() {
        for &(code, expected_value) in &[(1u32, 1i32), (2u32, 2i32)] {
            // Positive coefficient: sign bit 0.
            let bytes = pack(&[ue(code), (1, 0)]);
            let mut br = BitReader::new(&bytes);
            let coeff = read_chroma_dc_coefficient(&mut br).unwrap().unwrap();
            assert_eq!(coeff.run, 0);
            assert_eq!(coeff.value, expected_value);
        }
    }

    #[test]
    fn chroma_dc_code_3_yields_run_1_value_1() {
        // sign 1 → negative.
        let bytes = pack(&[ue(3), (1, 1)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_chroma_dc_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 1);
        assert_eq!(coeff.value, -1);
    }

    #[test]
    fn chroma_dc_code_4_yields_run_0_value_3() {
        // code 4: run = 4 & 0x3 = 0; value = ((4+9)>>2) - 0 = 3.
        let bytes = pack(&[ue(4), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_chroma_dc_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 0);
        assert_eq!(coeff.value, 3);
    }

    #[test]
    fn chroma_dc_code_5_yields_run_1_value_2() {
        // code 5: run = 5 & 0x3 = 1; value = ((5+9)>>2) - 1 = 3 - 1 = 2.
        let bytes = pack(&[ue(5), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_chroma_dc_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 1);
        assert_eq!(coeff.value, 2);
    }

    #[test]
    fn chroma_dc_code_6_yields_run_2_value_1() {
        // code 6: run = 6 & 0x3 = 2; value = ((6+9)>>2) - 2 = 3 - 2 = 1.
        let bytes = pack(&[ue(6), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_chroma_dc_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 2);
        assert_eq!(coeff.value, 1);
    }

    #[test]
    fn chroma_dc_code_7_yields_run_3_value_1() {
        // code 7: run = 7 & 0x3 = 3; value = ((7+9)>>2) - 3 = 4 - 3 = 1.
        let bytes = pack(&[ue(7), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_chroma_dc_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 3);
        assert_eq!(coeff.value, 1);
    }

    #[test]
    fn chroma_dc_code_8_yields_run_0_value_4() {
        // code 8: run = 8 & 0x3 = 0; value = ((8+9)>>2) - 0 = 4.
        let bytes = pack(&[ue(8), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_chroma_dc_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 0);
        assert_eq!(coeff.value, 4);
    }

    #[test]
    fn chroma_dc_truncated_at_golomb_errors() {
        // Empty bit stream — the Golomb decode hits Truncated before
        // it can produce a code.
        let mut br = BitReader::new(&[]);
        assert!(matches!(
            read_chroma_dc_coefficient(&mut br),
            Err(Error::Truncated)
        ));
    }

    // ---- Alt-scan single-coefficient walker ----------------------------

    #[test]
    fn alt_scan_code_0_signals_end_of_block() {
        let bytes = pack(&[ue(0)]);
        let mut br = BitReader::new(&bytes);
        assert!(read_alt_scan_coefficient(&mut br).unwrap().is_none());
    }

    #[test]
    fn alt_scan_explicit_codes_match_table() {
        for (code_idx, &(expected_run, expected_value)) in ALT_SCAN_TABLE_0_15.iter().enumerate() {
            if code_idx == 0 {
                continue; // end-of-block sentinel exercised above.
            }
            let code = code_idx as u32;
            let bytes = pack(&[ue(code), (1, 0)]);
            let mut br = BitReader::new(&bytes);
            let coeff = read_alt_scan_coefficient(&mut br).unwrap().unwrap();
            assert_eq!(coeff.run, expected_run, "code {code} run");
            assert_eq!(coeff.value, expected_value, "code {code} value");
        }
    }

    #[test]
    fn alt_scan_code_16_extension_formula() {
        // code 16: run = 16 & 0x7 = 0; value = (16 >> 3) - intra[0]
        //   = 2 - 8 = -6.
        let bytes = pack(&[ue(16), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_alt_scan_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 0);
        assert_eq!(coeff.value, -6);
    }

    #[test]
    fn alt_scan_code_17_extension_formula() {
        // code 17: run = 17 & 0x7 = 1; value = (17 >> 3) - intra[1]
        //   = 2 - 2 = 0.
        let bytes = pack(&[ue(17), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_alt_scan_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 1);
        assert_eq!(coeff.value, 0);
    }

    #[test]
    fn alt_scan_code_24_extension_formula() {
        // code 24: run = 24 & 0x7 = 0; value = (24 >> 3) - intra[0]
        //   = 3 - 8 = -5.
        let bytes = pack(&[ue(24), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_alt_scan_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 0);
        assert_eq!(coeff.value, -5);
    }

    #[test]
    fn alt_scan_code_with_high_run_uses_minus_ones_tail() {
        // run = 5 falls in the wiki spec's `[minus ones]` tail; the
        // correction is -1, so value = (code >> 3) - (-1) = (code >> 3)
        //  + 1.
        // Pick code such that code & 0x7 == 5: e.g. code = 21
        // (21 & 0x7 = 5; 21 >> 3 = 2; value = 2 - (-1) = 3).
        let bytes = pack(&[ue(21), (1, 1)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_alt_scan_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 5);
        assert_eq!(coeff.value, -3);
    }

    // ---- Normal-scan single-coefficient walker -------------------------

    #[test]
    fn normal_scan_code_0_signals_end_of_block() {
        let bytes = pack(&[ue(0)]);
        let mut br = BitReader::new(&bytes);
        assert!(read_normal_scan_coefficient(&mut br).unwrap().is_none());
    }

    #[test]
    fn normal_scan_explicit_codes_match_table() {
        for (code_idx, &(expected_run, expected_value)) in NORMAL_SCAN_TABLE_0_15.iter().enumerate()
        {
            if code_idx == 0 {
                continue;
            }
            let code = code_idx as u32;
            let bytes = pack(&[ue(code), (1, 0)]);
            let mut br = BitReader::new(&bytes);
            let coeff = read_normal_scan_coefficient(&mut br).unwrap().unwrap();
            assert_eq!(coeff.run, expected_run, "code {code} run");
            assert_eq!(coeff.value, expected_value, "code {code} value");
        }
    }

    #[test]
    fn normal_scan_code_16_extension_formula() {
        // code 16: run = 16 & 0xF = 0; value = (16 >> 4) - inter[0]
        //   = 1 - 4 = -3.
        let bytes = pack(&[ue(16), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_normal_scan_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 0);
        assert_eq!(coeff.value, -3);
    }

    #[test]
    fn normal_scan_code_17_extension_formula() {
        // code 17: run = 17 & 0xF = 1; value = (17 >> 4) - inter[1]
        //   = 1 - 2 = -1.
        let bytes = pack(&[ue(17), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_normal_scan_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 1);
        assert_eq!(coeff.value, -1);
    }

    #[test]
    fn normal_scan_code_32_extension_formula() {
        // code 32: run = 32 & 0xF = 0; value = (32 >> 4) - inter[0]
        //   = 2 - 4 = -2.
        let bytes = pack(&[ue(32), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_normal_scan_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 0);
        assert_eq!(coeff.value, -2);
    }

    #[test]
    fn normal_scan_high_run_uses_zeroes_tail() {
        // run = 10 falls in the wiki spec's `[zeroes]` tail; the
        // correction is 0, so value = (code >> 4).
        // code = 10 itself is in the explicit table (run=2, value=2);
        // pick code such that code & 0xF == 10 AND code >= 16: code
        // = 26 → run = 10, value = 26 >> 4 = 1.
        let bytes = pack(&[ue(26), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeff = read_normal_scan_coefficient(&mut br).unwrap().unwrap();
        assert_eq!(coeff.run, 10);
        assert_eq!(coeff.value, 1);
    }

    // ---- Block walker --------------------------------------------------

    #[test]
    fn chroma_dc_block_walks_to_end_sentinel() {
        // Block: code=1 sign=0 (run=0, value=1), code=2 sign=1
        // (run=0, value=-2), code=0 (end).
        let bytes = pack(&[ue(1), (1, 0), ue(2), (1, 1), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let coeffs = read_chroma_dc_block(&mut br).unwrap();
        assert_eq!(
            coeffs,
            vec![
                Coefficient { run: 0, value: 1 },
                Coefficient { run: 0, value: -2 },
            ]
        );
    }

    #[test]
    fn chroma_dc_block_walks_to_capacity_without_explicit_end() {
        // 4 successive run=0 value=1 coefficients fill the 2×2 block
        // without an explicit end-of-block sentinel.
        let bytes = pack(&[ue(1), (1, 0), ue(1), (1, 0), ue(1), (1, 0), ue(1), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let coeffs = read_chroma_dc_block(&mut br).unwrap();
        assert_eq!(coeffs.len(), 4);
        for c in &coeffs {
            assert_eq!(c.run, 0);
            assert_eq!(c.value, 1);
        }
    }

    #[test]
    fn chroma_dc_block_rejects_run_overflow() {
        // Build a code with run=2, value=1 (code=6) followed by another
        // code 6 → cursor goes 0+3=3 (OK, <= 4) then 3+3=6 > 4 → error.
        let bytes = pack(&[ue(6), (1, 0), ue(6), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let err = read_chroma_dc_block(&mut br).unwrap_err();
        assert!(matches!(err, Error::BadBitWidth(_)));
    }

    #[test]
    fn alt_scan_half_walks_to_end_sentinel() {
        // Half-scan: code 4 → (run=2, value=1), sign 0; code 0 end.
        let bytes = pack(&[ue(4), (1, 0), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let coeffs = read_alt_scan_half(&mut br).unwrap();
        assert_eq!(coeffs, vec![Coefficient { run: 2, value: 1 }]);
    }

    #[test]
    fn alt_scan_half_caps_at_eight_coefficients() {
        // 8 successive code=1 sign=0 coefficients fill the half-scan
        // (run=0, value=1 each, cursor 1..=8) without an explicit
        // sentinel.
        let mut packs: Vec<(u32, u32)> = Vec::new();
        for _ in 0..8 {
            packs.push(ue(1));
            packs.push((1, 0));
        }
        let bytes = pack(&packs);
        let mut br = BitReader::new(&bytes);
        let coeffs = read_alt_scan_half(&mut br).unwrap();
        assert_eq!(coeffs.len(), 8);
    }

    #[test]
    fn normal_scan_block_walks_to_end_sentinel() {
        // Block: code 1 → (run=0, value=1) sign 0; code 5 → (run=3,
        // value=1) sign 1; code 0 end.
        let bytes = pack(&[ue(1), (1, 0), ue(5), (1, 1), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let coeffs = read_normal_scan_block(&mut br).unwrap();
        assert_eq!(
            coeffs,
            vec![
                Coefficient { run: 0, value: 1 },
                Coefficient { run: 3, value: -1 },
            ]
        );
    }

    #[test]
    fn normal_scan_block_caps_at_sixteen_coefficients() {
        // 16 successive code=1 sign=0 coefficients fill the 4×4 block.
        let mut packs: Vec<(u32, u32)> = Vec::new();
        for _ in 0..16 {
            packs.push(ue(1));
            packs.push((1, 0));
        }
        let bytes = pack(&packs);
        let mut br = BitReader::new(&bytes);
        let coeffs = read_normal_scan_block(&mut br).unwrap();
        assert_eq!(coeffs.len(), 16);
    }

    #[test]
    fn normal_scan_block_rejects_run_overflow() {
        // code 14 → (run=9, value=1), cursor 0+10=10. Then code 14
        // again → run=9, cursor 10+10=20 > 16 → BadBitWidth.
        let bytes = pack(&[ue(14), (1, 0), ue(14), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let err = read_normal_scan_block(&mut br).unwrap_err();
        assert!(matches!(err, Error::BadBitWidth(_)));
    }

    #[test]
    fn block_walker_truncated_propagates() {
        // Empty bit stream — first Golomb decode hits Truncated.
        let mut br = BitReader::new(&[]);
        assert!(matches!(
            read_normal_scan_block(&mut br),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn empty_block_with_immediate_end_sentinel() {
        // Just an end-of-block code → empty coefficient list.
        let bytes = pack(&[ue(0)]);
        let mut br = BitReader::new(&bytes);
        let coeffs = read_normal_scan_block(&mut br).unwrap();
        assert!(coeffs.is_empty());
    }

    // ---- Alt-scan two-half block walker --------------------------------

    #[test]
    fn alt_scan_block_both_halves_empty() {
        // Two consecutive end-of-block sentinels → both halves empty.
        let bytes = pack(&[ue(0), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let block = read_alt_scan_block(&mut br).unwrap();
        assert!(block.first_half.is_empty());
        assert!(block.second_half.is_empty());
        assert!(block.is_empty());
        assert_eq!(block.len(), 0);
    }

    #[test]
    fn alt_scan_block_first_half_only_populated() {
        // First half: code 1 → (run=0, value=1), sign 0; code 0 end.
        // Second half: code 0 end immediately.
        let bytes = pack(&[ue(1), (1, 0), ue(0), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let block = read_alt_scan_block(&mut br).unwrap();
        assert_eq!(block.first_half, vec![Coefficient { run: 0, value: 1 }]);
        assert!(block.second_half.is_empty());
        assert_eq!(block.len(), 1);
        assert!(!block.is_empty());
    }

    #[test]
    fn alt_scan_block_second_half_only_populated() {
        // First half: code 0 end immediately.
        // Second half: code 2 → (run=1, value=1), sign 1; code 0 end.
        let bytes = pack(&[ue(0), ue(2), (1, 1), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let block = read_alt_scan_block(&mut br).unwrap();
        assert!(block.first_half.is_empty());
        assert_eq!(block.second_half, vec![Coefficient { run: 1, value: -1 }]);
        assert_eq!(block.len(), 1);
    }

    #[test]
    fn alt_scan_block_both_halves_capped_at_eight_each() {
        // 8 coeffs per half × 2 halves = 16 coefficients total without
        // any explicit end-of-block sentinel between or after. Each
        // coefficient is code=1 sign=0 → (run=0, value=1) which advances
        // each cursor by exactly 1 per coefficient.
        let mut packs: Vec<(u32, u32)> = Vec::new();
        for _ in 0..16 {
            packs.push(ue(1));
            packs.push((1, 0));
        }
        let bytes = pack(&packs);
        let mut br = BitReader::new(&bytes);
        let block = read_alt_scan_block(&mut br).unwrap();
        assert_eq!(block.first_half.len(), COEFFS_PER_ALT_SCAN_HALF);
        assert_eq!(block.second_half.len(), COEFFS_PER_ALT_SCAN_HALF);
        assert_eq!(block.len(), COEFFS_PER_4X4_BLOCK);
        for c in block.first_half.iter().chain(block.second_half.iter()) {
            assert_eq!(c.run, 0);
            assert_eq!(c.value, 1);
        }
    }

    #[test]
    fn alt_scan_block_halves_use_independent_cursors() {
        // First half: a single code=7 → (run=0, value=5) sign 0; cursor
        // advances to 1. Then code 0 → end of first half. Second half:
        // start fresh at cursor 0 — a code 8 → (run=3, value=1) sign 0
        // places at scan position 3, cursor advances to 4. Then code 0
        // ends. If the cursor had carried over, the second half would
        // observe cursor 1 + 4 = 5 instead of 4 — the AltScanBlock
        // shape captures the per-half independence.
        let bytes = pack(&[ue(7), (1, 0), ue(0), ue(8), (1, 0), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let block = read_alt_scan_block(&mut br).unwrap();
        assert_eq!(block.first_half, vec![Coefficient { run: 0, value: 5 }]);
        assert_eq!(block.second_half, vec![Coefficient { run: 3, value: 1 }]);
    }

    #[test]
    fn alt_scan_block_first_half_at_capacity_does_not_consume_second_half_bits() {
        // Fill the first half to capacity (8 code=1 sign=0 entries) so
        // it returns without consuming an end-of-block sentinel. Then
        // the second half consumes the very next code: code 4 →
        // (run=2, value=1) sign 1; code 0 end. This pins that the
        // capacity-cap exit from the first half does NOT eat the first
        // bit of the second half.
        let mut packs: Vec<(u32, u32)> = Vec::new();
        for _ in 0..8 {
            packs.push(ue(1));
            packs.push((1, 0));
        }
        packs.push(ue(4));
        packs.push((1, 1));
        packs.push(ue(0));
        let bytes = pack(&packs);
        let mut br = BitReader::new(&bytes);
        let block = read_alt_scan_block(&mut br).unwrap();
        assert_eq!(block.first_half.len(), COEFFS_PER_ALT_SCAN_HALF);
        assert_eq!(block.second_half, vec![Coefficient { run: 2, value: -1 }]);
    }

    #[test]
    fn alt_scan_block_run_overflow_in_first_half_propagates() {
        // code 8 → (run=3, value=1), cursor 0+4=4. code 9 → (run=4,
        // value=1), cursor 4+5=9 > 8 → BadBitWidth before the second
        // half is touched.
        let bytes = pack(&[ue(8), (1, 0), ue(9), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let err = read_alt_scan_block(&mut br).unwrap_err();
        assert!(matches!(err, Error::BadBitWidth(_)));
    }

    #[test]
    fn alt_scan_block_run_overflow_in_second_half_propagates() {
        // First half: a single code 1 sign 0 then end. Second half:
        // code 8 → (run=3, value=1), cursor 0+4=4. code 9 → (run=4,
        // value=1), cursor 4+5=9 > 8 → BadBitWidth.
        let bytes = pack(&[ue(1), (1, 0), ue(0), ue(8), (1, 0), ue(9), (1, 0)]);
        let mut br = BitReader::new(&bytes);
        let err = read_alt_scan_block(&mut br).unwrap_err();
        assert!(matches!(err, Error::BadBitWidth(_)));
    }

    #[test]
    fn alt_scan_block_truncated_in_first_half_propagates() {
        // Empty bit stream → first Golomb decode hits Truncated.
        let mut br = BitReader::new(&[]);
        assert!(matches!(
            read_alt_scan_block(&mut br),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn alt_scan_block_truncated_in_second_half_propagates() {
        // First half terminates cleanly (just ue(0)). Second half hits
        // end-of-stream on its first Golomb decode → Truncated.
        let bytes = pack(&[ue(0)]);
        let mut br = BitReader::new(&bytes);
        assert!(matches!(
            read_alt_scan_block(&mut br),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn alt_scan_block_flatten_preserves_stream_order() {
        // First half: code 1 sign 0; second half: code 2 sign 1; ends.
        let bytes = pack(&[ue(1), (1, 0), ue(0), ue(2), (1, 1), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let block = read_alt_scan_block(&mut br).unwrap();
        assert_eq!(
            block.flatten(),
            vec![
                Coefficient { run: 0, value: 1 },
                Coefficient { run: 1, value: -1 },
            ]
        );
    }

    #[test]
    fn alt_scan_block_iter_with_half_yields_indexed_pairs() {
        // First half: code 1 sign 0; second half: code 1 sign 0; ends.
        let bytes = pack(&[ue(1), (1, 0), ue(0), ue(1), (1, 0), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let block = read_alt_scan_block(&mut br).unwrap();
        let collected: Vec<(u8, Coefficient)> = block.iter_with_half().collect();
        assert_eq!(
            collected,
            vec![
                (0, Coefficient { run: 0, value: 1 }),
                (1, Coefficient { run: 0, value: 1 }),
            ]
        );
    }

    #[test]
    fn alt_scan_block_total_length_bounded_by_full_4x4_capacity() {
        // The block's total non-zero coefficient count is bounded by
        // 2 × COEFFS_PER_ALT_SCAN_HALF = COEFFS_PER_4X4_BLOCK. Pin this
        // bound as a structural invariant.
        assert_eq!(
            2 * COEFFS_PER_ALT_SCAN_HALF,
            COEFFS_PER_4X4_BLOCK,
            "alt-scan two-half capacity must equal the 4×4 block capacity"
        );
    }
}
