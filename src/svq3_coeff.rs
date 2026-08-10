//! SVQ3 residual (level, run) code books and block decoders.
//!
//! Implements `docs/video/svq3/spec/06-residual-coefficient-coding.md`
//! §2/§3/§5: a residual block is a sequence of universal-code code
//! numbers ([`crate::svq3::read_universal_code`]); each code number
//! either ends the block (code 0) or yields a **(level, run)** pair —
//! `run` zero coefficients are skipped along the scan before `level`
//! is stored.
//!
//! The three code books are transcribed from
//! `docs/video/svq3/tables/05-residual-codebooks.csv`; every code
//! number at or above a book's escape threshold is constructed
//! arithmetically from the per-run constants in
//! `docs/video/svq3/tables/06-residual-escape-level-base.csv` (the
//! constructions are written out in that table's `.meta`). In all
//! three books the sign is the low bit of the code number — odd
//! positive, even negative — both inside the tabulated range and in
//! the escape range.
//!
//! Which book a block uses (spec/06 §2):
//!
//! | Block | Book |
//! | ----- | ---- |
//! | luma 4×4 of an intra 4×4 MB, quantiser < 24 | [`ALTERNATE_SCAN_BOOK`] |
//! | luma 4×4 of an intra 4×4 MB, quantiser ≥ 24 | [`NORMAL_SCAN_BOOK`] |
//! | luma 4×4 of an inter MB | [`NORMAL_SCAN_BOOK`] |
//! | separate luma DC block + every luma 4×4 of an intra 16×16 MB | [`NORMAL_SCAN_BOOK`] |
//! | chroma 4×4 AC block, any MB | [`NORMAL_SCAN_BOOK`] |
//! | 2×2 chroma DC block | [`CHROMA_DC_BOOK`] |
//!
//! The block decoders here store **raw levels** at the scan-mapped
//! raster positions; the one-multiply dequantisation (spec/06 §4,
//! `coefficient = level × dequant[quantiser_index]`) is applied by the
//! reconstruction layer so a single decode path serves the luma /
//! chroma quantiser-index split (spec/04 §3) and the intra-luma
//! inline-DC scale.
//!
//! Bounds behaviour follows spec/06 §5: each decoder carries an
//! explicit bound on the coefficient index it is about to write and
//! treats a violation as a bitstream error rather than wrapping or
//! clamping.
//!
//! The alternate-scan block is read as **two independent half-scans**
//! of up to eight coefficients each (scan positions `0..8` then
//! `8..16`): the wiki snapshot §"Coefficient decoding" states the
//! block "is coded in two parts of up to eight coefficients
//! corresponding to each half-scan", the staged alternate scan order
//! splits exactly into two 8-entry halves (spec/01 Gap 1), and the
//! alternate book's escape run mask is 7 (tables/06 `.meta`) — a run
//! can never span more than one 8-position half, which is only
//! consistent with per-half coding.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::svq3::read_universal_code;

/// A single decoded `(run, signed_value)` coefficient pair, kept for
/// the placement helpers in [`crate::svq3_scan`].
///
/// `run` is the number of zero coefficients **preceding** this entry
/// along the scan; `value` is the signed level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coefficient {
    /// Number of zero coefficients preceding this entry in the scan
    /// order.
    pub run: u32,
    /// Signed coefficient level.
    pub value: i32,
}

/// The three residual code books of spec/06 §2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidualBook {
    /// The normal-zigzag book — every residual block except the two
    /// cases below.
    NormalScan,
    /// The alternate-scan book — luma 4×4 blocks of an intra 4×4
    /// macroblock whose quantiser is below 24.
    AlternateScan,
    /// The 2×2 chroma DC book.
    ChromaDc,
}

/// Tabulated `(level, run)` pairs of the `normal_scan` book, keyed by
/// code number (`docs/video/svq3/tables/05-residual-codebooks.csv`).
/// Entry 0 is the end-of-block symbol. Codes ≥ [`NORMAL_SCAN_ESCAPE`]
/// are constructed arithmetically.
pub const NORMAL_SCAN_BOOK: [(i8, u8); 31] = [
    (0, 0),
    (1, 0),
    (-1, 0),
    (1, 1),
    (-1, 1),
    (1, 2),
    (-1, 2),
    (2, 0),
    (-2, 0),
    (1, 3),
    (-1, 3),
    (1, 4),
    (-1, 4),
    (1, 5),
    (-1, 5),
    (3, 0),
    (-3, 0),
    (2, 1),
    (-2, 1),
    (2, 2),
    (-2, 2),
    (1, 6),
    (-1, 6),
    (1, 7),
    (-1, 7),
    (1, 8),
    (-1, 8),
    (1, 9),
    (-1, 9),
    (4, 0),
    (-4, 0),
];

/// Tabulated `(level, run)` pairs of the `alternate_scan` book
/// (`docs/video/svq3/tables/05-residual-codebooks.csv`). Entry 0 is
/// the end-of-block symbol. Codes ≥ [`ALTERNATE_SCAN_ESCAPE`] are
/// constructed arithmetically.
pub const ALTERNATE_SCAN_BOOK: [(i8, u8); 31] = [
    (0, 0),
    (1, 0),
    (-1, 0),
    (1, 1),
    (-1, 1),
    (2, 0),
    (-2, 0),
    (1, 2),
    (-1, 2),
    (3, 0),
    (-3, 0),
    (4, 0),
    (-4, 0),
    (5, 0),
    (-5, 0),
    (1, 3),
    (-1, 3),
    (1, 4),
    (-1, 4),
    (2, 1),
    (-2, 1),
    (3, 1),
    (-3, 1),
    (6, 0),
    (-6, 0),
    (7, 0),
    (-7, 0),
    (8, 0),
    (-8, 0),
    (9, 0),
    (-9, 0),
];

/// Tabulated `(level, run)` pairs of the `chroma_dc` book
/// (`docs/video/svq3/tables/05-residual-codebooks.csv`). Entry 0 is
/// the end-of-block symbol. Codes ≥ [`CHROMA_DC_ESCAPE`] are
/// constructed arithmetically.
pub const CHROMA_DC_BOOK: [(i8, u8); 23] = [
    (0, 0),
    (1, 0),
    (-1, 0),
    (2, 0),
    (-2, 0),
    (1, 1),
    (-1, 1),
    (3, 0),
    (-3, 0),
    (2, 1),
    (-2, 1),
    (1, 2),
    (-1, 2),
    (1, 3),
    (-1, 3),
    (4, 0),
    (-4, 0),
    (3, 1),
    (-3, 1),
    (2, 2),
    (-2, 2),
    (2, 3),
    (-2, 3),
];

/// Escape threshold of the `normal_scan` book: code numbers at or
/// above this are constructed arithmetically
/// (`docs/video/svq3/tables/05-residual-codebooks.csv` extent notes).
pub const NORMAL_SCAN_ESCAPE: u32 = 31;

/// Escape threshold of the `alternate_scan` book.
pub const ALTERNATE_SCAN_ESCAPE: u32 = 31;

/// Escape threshold of the `chroma_dc` book.
pub const CHROMA_DC_ESCAPE: u32 = 23;

/// Per-run level-magnitude bases of the `normal_scan` escape range
/// (`docs/video/svq3/tables/06-residual-escape-level-base.csv`): the
/// smallest level magnitude the escape range represents at that run.
pub const NORMAL_SCAN_ESCAPE_LEVEL_BASE: [i32; 16] =
    [5, 3, 3, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1];

/// Per-run level-magnitude bases of the `alternate_scan` escape range
/// (`docs/video/svq3/tables/06-residual-escape-level-base.csv`).
pub const ALTERNATE_SCAN_ESCAPE_LEVEL_BASE: [i32; 8] = [10, 4, 2, 2, 2, 1, 1, 1];

/// Per-run code-number bases of the `chroma_dc` escape range
/// (`docs/video/svq3/tables/06-residual-escape-level-base.csv`): the
/// code-number base subtracted before the shift; the starting level
/// magnitude is the literal 3.
pub const CHROMA_DC_ESCAPE_CODE_BASE: [u32; 4] = [7, 17, 27, 29];

/// Resolve one code number against a residual book.
///
/// Returns `Ok(None)` for the end-of-block symbol (code 0) and
/// `Ok(Some((level, run)))` otherwise. Code numbers below the book's
/// escape threshold are direct table lookups; those at or above it use
/// the arithmetic constructions written out in
/// `docs/video/svq3/tables/06-residual-escape-level-base.csv.meta`:
///
/// ```text
/// normal_scan     (C >= 31)  run = ((C + 1) >> 1) & 15
///                            mag = ((C - 31) >> 5) + base_normal[run]
/// alternate_scan  (C >= 31)  run = ((C + 1) >> 1) & 7
///                            mag = ((C - 31) >> 4) + base_alternate[run]
/// chroma_dc       (C >= 23)  run = ((C + 1) >> 1) & 3
///                            mag = ((C - base_chroma[run]) >> 3) + 3
/// ```
///
/// with the sign carried by the low bit of the code number throughout
/// (odd positive, even negative).
#[must_use]
pub fn resolve_level_run(book: ResidualBook, code: u32) -> Option<(i32, u32)> {
    if code == 0 {
        return None;
    }
    let (level, run): (i32, u32) = match book {
        ResidualBook::NormalScan => {
            if code < NORMAL_SCAN_ESCAPE {
                let (l, r) = NORMAL_SCAN_BOOK[code as usize];
                (l as i32, r as u32)
            } else {
                let run = ((code + 1) >> 1) & 15;
                let mag = (((code - NORMAL_SCAN_ESCAPE) >> 5) as i32)
                    + NORMAL_SCAN_ESCAPE_LEVEL_BASE[run as usize];
                (if code & 1 == 1 { mag } else { -mag }, run)
            }
        }
        ResidualBook::AlternateScan => {
            if code < ALTERNATE_SCAN_ESCAPE {
                let (l, r) = ALTERNATE_SCAN_BOOK[code as usize];
                (l as i32, r as u32)
            } else {
                let run = ((code + 1) >> 1) & 7;
                let mag = (((code - ALTERNATE_SCAN_ESCAPE) >> 4) as i32)
                    + ALTERNATE_SCAN_ESCAPE_LEVEL_BASE[run as usize];
                (if code & 1 == 1 { mag } else { -mag }, run)
            }
        }
        ResidualBook::ChromaDc => {
            if code < CHROMA_DC_ESCAPE {
                let (l, r) = CHROMA_DC_BOOK[code as usize];
                (l as i32, r as u32)
            } else {
                let run = ((code + 1) >> 1) & 3;
                // The subtraction cannot underflow for any reachable
                // (code, run) combination: the smallest escape code of
                // each run class equals that run's code base + 0..1.
                let mag =
                    ((code.wrapping_sub(CHROMA_DC_ESCAPE_CODE_BASE[run as usize]) >> 3) as i32) + 3;
                (if code & 1 == 1 { mag } else { -mag }, run)
            }
        }
    };
    Some((level, run))
}

/// Decode one residual 4×4 block through the `normal_scan` book,
/// storing **raw levels** at the raster positions `scan` maps the
/// reached scan positions to.
///
/// * `scan` — the destination map (scan position → raster index),
///   normally [`crate::svq3_scan::NORMAL_ZIGZAG_4X4_SCAN`].
/// * `start` — the first scan position (0 for a block whose DC is
///   inline; 1 for an AC-only pass whose position 0 is owned by a
///   separate DC path, spec/04 §4.3).
/// * `out` — cleared to zero first (spec/06 §2: "each decoder clears
///   the destination block before it starts").
///
/// Reads code numbers until the end-of-block symbol; a run that would
/// carry the scan position past the end of the block is a bitstream
/// error (spec/06 §5). Returns the scan position reached — `start`
/// exactly when no coefficient was decoded, which is the "DC only"
/// observable spec/04 §4.3 keys on.
pub fn decode_residual_4x4_normal(
    br: &mut BitReader<'_>,
    scan: &[usize; 16],
    start: usize,
    out: &mut [i32; 16],
) -> Result<usize> {
    debug_assert!(start < 16);
    *out = [0; 16];
    let mut pos = start;
    loop {
        let code = read_universal_code(br)?;
        let Some((level, run)) = resolve_level_run(ResidualBook::NormalScan, code) else {
            return Ok(pos);
        };
        pos = pos
            .checked_add(run as usize)
            .ok_or(Error::BadBitWidth(run))?;
        if pos >= 16 {
            return Err(Error::BadBitWidth(pos as u32));
        }
        out[scan[pos]] = level;
        pos += 1;
        if pos == 16 {
            return Ok(pos);
        }
    }
}

/// Decode one intra-4×4 luma residual block through the
/// `alternate_scan` book: **two independent half-scans** of up to
/// eight coefficients each (see the module docs for why the block is
/// two-part).
///
/// The first half covers scan positions `0..8`, the second `8..16`,
/// both mapped through [`crate::svq3_scan::ALT_SCAN_4X4_SCAN`]-style
/// destination maps; each half's runs are relative to that half and
/// each half is terminated by its own end-of-block symbol (or by
/// filling all eight of its positions). `out` is cleared first.
///
/// Returns the total number of non-zero coefficients decoded.
pub fn decode_residual_4x4_alt(
    br: &mut BitReader<'_>,
    scan: &[usize; 16],
    out: &mut [i32; 16],
) -> Result<usize> {
    *out = [0; 16];
    let mut count = 0usize;
    for half in 0..2usize {
        let base = half * 8;
        let mut pos = 0usize;
        loop {
            let code = read_universal_code(br)?;
            let Some((level, run)) = resolve_level_run(ResidualBook::AlternateScan, code) else {
                break;
            };
            pos = pos
                .checked_add(run as usize)
                .ok_or(Error::BadBitWidth(run))?;
            if pos >= 8 {
                return Err(Error::BadBitWidth(pos as u32));
            }
            out[scan[base + pos]] = level;
            count += 1;
            pos += 1;
            if pos == 8 {
                break;
            }
        }
    }
    Ok(count)
}

/// Decode the 2×2 chroma DC block through the `chroma_dc` book.
///
/// The four positions are addressed directly — no scan array — and a
/// run advances the position by one per skipped coefficient
/// (spec/06 §3). The coded order is the 2×2 raster
/// `[[c0, c1], [c2, c3]]` (spec/04 §2.2). `out` is cleared first;
/// raw levels are stored. Returns the position reached.
pub fn decode_chroma_dc_2x2(br: &mut BitReader<'_>, out: &mut [i32; 4]) -> Result<usize> {
    *out = [0; 4];
    let mut pos = 0usize;
    loop {
        let code = read_universal_code(br)?;
        let Some((level, run)) = resolve_level_run(ResidualBook::ChromaDc, code) else {
            return Ok(pos);
        };
        pos = pos
            .checked_add(run as usize)
            .ok_or(Error::BadBitWidth(run))?;
        if pos >= 4 {
            return Err(Error::BadBitWidth(pos as u32));
        }
        out[pos] = level;
        pos += 1;
        if pos == 4 {
            return Ok(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svq3_scan::{ALT_SCAN_4X4_SCAN, NORMAL_ZIGZAG_4X4_SCAN};

    /// Pack `(width, value)` items into bytes, MSB-first.
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

    /// Universal-code encoding helper (spec/06 §1 interleaved layout).
    fn ue(n: u32) -> (u32, u32) {
        let exp = 31 - (n + 1).leading_zeros();
        let data = n + 1 - (1u32 << exp);
        match exp {
            0 => (1, 1),
            1 => (3, 0b010 | data),
            _ => {
                let mut bits: u32 = 0b00;
                bits = (bits << 1) | ((data >> (exp - 1)) & 1);
                bits = (bits << 1) | ((data >> (exp - 2)) & 1);
                let mut width = 4;
                for i in (0..exp - 2).rev() {
                    bits = (bits << 2) | ((data >> i) & 1);
                    width += 2;
                }
                (width + 1, (bits << 1) | 1)
            }
        }
    }

    // ---- Book-shape invariants (tables/05 validation notes) ----------

    #[test]
    fn books_start_with_end_of_block() {
        assert_eq!(NORMAL_SCAN_BOOK[0], (0, 0));
        assert_eq!(ALTERNATE_SCAN_BOOK[0], (0, 0));
        assert_eq!(CHROMA_DC_BOOK[0], (0, 0));
    }

    /// tables/05 validation: every entry past 0 occurs in consecutive
    /// `+v` / `−v` pairs at odd/even code numbers, and no `(level,
    /// run)` combination repeats within a book.
    #[test]
    fn books_are_signed_pairs_without_repeats() {
        fn check(book: &[(i8, u8)]) {
            let mut seen: Vec<(i8, u8)> = Vec::new();
            let mut idx = 1;
            while idx + 1 < book.len() + 1 && idx < book.len() {
                let (l, r) = book[idx];
                assert!(l > 0, "odd code {idx} must be positive");
                if idx + 1 < book.len() {
                    assert_eq!(book[idx + 1], (-l, r), "even code {} pairs", idx + 1);
                }
                assert!(!seen.contains(&(l, r)), "repeat ({l},{r})");
                seen.push((l, r));
                idx += 2;
            }
        }
        check(&NORMAL_SCAN_BOOK);
        check(&ALTERNATE_SCAN_BOOK);
        check(&CHROMA_DC_BOOK);
    }

    /// The two 4×4 books share the short codes ±1/run-0 (tables/05
    /// cross-book check) and are distinct books overall.
    #[test]
    fn four_by_four_books_share_short_codes() {
        assert_eq!(NORMAL_SCAN_BOOK[1], ALTERNATE_SCAN_BOOK[1]);
        assert_eq!(NORMAL_SCAN_BOOK[2], ALTERNATE_SCAN_BOOK[2]);
        assert_ne!(NORMAL_SCAN_BOOK, ALTERNATE_SCAN_BOOK);
        assert_ne!(NORMAL_SCAN_BOOK[5], ALTERNATE_SCAN_BOOK[5]);
    }

    // ---- Escape continuity (tables/06 validation) --------------------

    /// For each (book, run) pair the first escape code continues the
    /// tabulated magnitude sequence by exactly one step, positive sign,
    /// no overlap — the joint check tables/06 describes.
    #[test]
    fn escape_ranges_continue_tabulated_magnitudes() {
        for (book, table, escape, max_run) in [
            (
                ResidualBook::NormalScan,
                &NORMAL_SCAN_BOOK[..],
                NORMAL_SCAN_ESCAPE,
                15u32,
            ),
            (
                ResidualBook::AlternateScan,
                &ALTERNATE_SCAN_BOOK[..],
                ALTERNATE_SCAN_ESCAPE,
                7,
            ),
            (
                ResidualBook::ChromaDc,
                &CHROMA_DC_BOOK[..],
                CHROMA_DC_ESCAPE,
                3,
            ),
        ] {
            for run in 0..=max_run {
                // Largest tabulated magnitude at this run.
                let max_tab = table
                    .iter()
                    .filter(|&&(l, r)| r as u32 == run && l > 0)
                    .map(|&(l, _)| l as i32)
                    .max()
                    .unwrap_or(0);
                // Smallest escape code with this run: scan upward.
                let mut first: Option<(u32, i32)> = None;
                for code in escape..escape + 64 {
                    if let Some((level, r)) = resolve_level_run(book, code) {
                        if r == run && level > 0 {
                            first = Some((code, level));
                            break;
                        }
                    }
                }
                let (code, level) = first.expect("escape range covers every run class");
                assert_eq!(
                    level,
                    max_tab + 1,
                    "book {book:?} run {run}: escape (code {code}) must continue the ladder"
                );
                // Its even sibling is the negative of the same magnitude.
                let (neg, r2) = resolve_level_run(book, code + 1).unwrap();
                assert_eq!((neg, r2), (-level, run));
            }
        }
    }

    /// Worked chroma-DC example from tables/06 `.meta`: code 23 →
    /// (+5, 0), code 24 → (−5, 0), continuing the ±4 at codes 15/16.
    #[test]
    fn chroma_dc_escape_worked_example() {
        assert_eq!(resolve_level_run(ResidualBook::ChromaDc, 15), Some((4, 0)));
        assert_eq!(resolve_level_run(ResidualBook::ChromaDc, 16), Some((-4, 0)));
        assert_eq!(resolve_level_run(ResidualBook::ChromaDc, 23), Some((5, 0)));
        assert_eq!(resolve_level_run(ResidualBook::ChromaDc, 24), Some((-5, 0)));
    }

    /// Escape magnitude growth: each +32 (normal), +16 (alternate) or
    /// +8 (chroma) of code number raises the magnitude by one within a
    /// run class.
    #[test]
    fn escape_magnitude_steps() {
        let (l0, _) = resolve_level_run(ResidualBook::NormalScan, 31).unwrap();
        let (l1, _) = resolve_level_run(ResidualBook::NormalScan, 63).unwrap();
        assert_eq!(l1, l0 + 1);
        let (a0, _) = resolve_level_run(ResidualBook::AlternateScan, 31).unwrap();
        let (a1, _) = resolve_level_run(ResidualBook::AlternateScan, 47).unwrap();
        assert_eq!(a1, a0 + 1);
        let (c0, _) = resolve_level_run(ResidualBook::ChromaDc, 23).unwrap();
        let (c1, _) = resolve_level_run(ResidualBook::ChromaDc, 31).unwrap();
        assert_eq!(c1, c0 + 1);
    }

    /// A maximum-size code number flows through every book without
    /// overflow (hostile-input hardening).
    #[test]
    fn chroma_dc_extension_survives_maximum_golomb_code() {
        let code = u32::MAX - 1;
        for book in [
            ResidualBook::NormalScan,
            ResidualBook::AlternateScan,
            ResidualBook::ChromaDc,
        ] {
            let (level, run) = resolve_level_run(book, code).unwrap();
            assert!(level < 0, "even code is negative ({book:?})");
            assert!(run <= 15);
        }
    }

    // ---- Block decoders ----------------------------------------------

    #[test]
    fn normal_block_places_levels_along_the_zigzag() {
        // code 1 = (+1, 0) → scan pos 0 → raster 0;
        // code 17 = (+2, 1) → skip pos 1, store at pos 2 → raster 4;
        // code 0 ends.
        let bytes = pack(&[ue(1), ue(17), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let mut out = [99i32; 16];
        let pos = decode_residual_4x4_normal(&mut br, &NORMAL_ZIGZAG_4X4_SCAN, 0, &mut out)
            .expect("decode");
        assert_eq!(pos, 3);
        let mut expected = [0i32; 16];
        expected[NORMAL_ZIGZAG_4X4_SCAN[0]] = 1;
        expected[NORMAL_ZIGZAG_4X4_SCAN[2]] = 2;
        assert_eq!(out, expected);
    }

    #[test]
    fn normal_block_start_1_leaves_dc_untouched() {
        // AC-only pass (spec/04 §4.3): start = 1. code 1 = (+1, 0)
        // stores at scan pos 1 → raster NORMAL_ZIGZAG_4X4_SCAN[1].
        let bytes = pack(&[ue(1), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let mut out = [0i32; 16];
        let pos = decode_residual_4x4_normal(&mut br, &NORMAL_ZIGZAG_4X4_SCAN, 1, &mut out)
            .expect("decode");
        assert_eq!(pos, 2);
        assert_eq!(out[NORMAL_ZIGZAG_4X4_SCAN[1]], 1);
        assert_eq!(out[0], 0, "DC slot untouched by the AC pass");
    }

    #[test]
    fn normal_block_empty_returns_start() {
        let bytes = pack(&[ue(0)]);
        let mut br = BitReader::new(&bytes);
        let mut out = [0i32; 16];
        let pos = decode_residual_4x4_normal(&mut br, &NORMAL_ZIGZAG_4X4_SCAN, 1, &mut out)
            .expect("decode");
        assert_eq!(pos, 1, "spec/04 §4.3: position 1 = no AC decoded");
        assert_eq!(out, [0i32; 16]);
    }

    #[test]
    fn normal_block_run_overflow_errors() {
        // code 27 = (+1, 9): pos 0+9=9 store, pos=10. Again: 10+9=19
        // ≥ 16 → bitstream error (spec/06 §5).
        let bytes = pack(&[ue(27), ue(27)]);
        let mut br = BitReader::new(&bytes);
        let mut out = [0i32; 16];
        assert!(matches!(
            decode_residual_4x4_normal(&mut br, &NORMAL_ZIGZAG_4X4_SCAN, 0, &mut out),
            Err(Error::BadBitWidth(_))
        ));
    }

    #[test]
    fn normal_block_fills_to_capacity_without_trailing_symbol() {
        // Sixteen (+1, 0) codes fill positions 0..16; the decoder must
        // stop at the bound without demanding a further symbol.
        let items: Vec<(u32, u32)> = (0..16).map(|_| ue(1)).collect();
        let bytes = pack(&items);
        let mut br = BitReader::new(&bytes);
        let mut out = [0i32; 16];
        let pos = decode_residual_4x4_normal(&mut br, &NORMAL_ZIGZAG_4X4_SCAN, 0, &mut out)
            .expect("decode");
        assert_eq!(pos, 16);
        assert_eq!(out, [1i32; 16]);
        assert_eq!(br.bits_remaining(), 0, "no end-of-block symbol consumed");
    }

    #[test]
    fn alt_block_two_halves_are_independent() {
        // First half: code 1 = (+1, 0) at half-pos 0 → scan[0]; end.
        // Second half: code 3 = (+1, 1) skips half-pos 0, stores at
        // half-pos 1 → scan[8 + 1]; end.
        let bytes = pack(&[ue(1), ue(0), ue(3), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let mut out = [0i32; 16];
        let n = decode_residual_4x4_alt(&mut br, &ALT_SCAN_4X4_SCAN, &mut out).expect("decode");
        assert_eq!(n, 2);
        let mut expected = [0i32; 16];
        expected[ALT_SCAN_4X4_SCAN[0]] = 1;
        expected[ALT_SCAN_4X4_SCAN[9]] = 1;
        assert_eq!(out, expected);
    }

    #[test]
    fn alt_block_half_overflow_errors() {
        // code 17 = (+1, 4): pos 4, then again: 5+4=9 ≥ 8 → error.
        let bytes = pack(&[ue(17), ue(17)]);
        let mut br = BitReader::new(&bytes);
        let mut out = [0i32; 16];
        assert!(matches!(
            decode_residual_4x4_alt(&mut br, &ALT_SCAN_4X4_SCAN, &mut out),
            Err(Error::BadBitWidth(_))
        ));
    }

    #[test]
    fn alt_block_full_halves_consume_no_end_symbol() {
        // Eight (+1, 0) codes fill the first half, eight more the
        // second; no end-of-block symbols anywhere.
        let items: Vec<(u32, u32)> = (0..16).map(|_| ue(1)).collect();
        let bytes = pack(&items);
        let mut br = BitReader::new(&bytes);
        let mut out = [0i32; 16];
        let n = decode_residual_4x4_alt(&mut br, &ALT_SCAN_4X4_SCAN, &mut out).expect("decode");
        assert_eq!(n, 16);
        assert_eq!(out, [1i32; 16]);
        assert_eq!(br.bits_remaining(), 0);
    }

    #[test]
    fn chroma_dc_block_direct_positions() {
        // code 5 = (+1, 1): skip pos 0, store at pos 1; code 3 =
        // (+2, 0): store at pos 2; end.
        let bytes = pack(&[ue(5), ue(3), ue(0)]);
        let mut br = BitReader::new(&bytes);
        let mut out = [0i32; 4];
        let pos = decode_chroma_dc_2x2(&mut br, &mut out).expect("decode");
        assert_eq!(pos, 3);
        assert_eq!(out, [0, 1, 2, 0]);
    }

    #[test]
    fn chroma_dc_block_overflow_errors() {
        // code 21 = (+2, 3): pos 3 store, pos 4 → full. A second
        // coefficient beforehand pushes past: code 5 = (+1,1) → pos 1,
        // pos=2; then code 21 = (+2,3): 2+3=5 ≥ 4 → error.
        let bytes = pack(&[ue(5), ue(21)]);
        let mut br = BitReader::new(&bytes);
        let mut out = [0i32; 4];
        assert!(matches!(
            decode_chroma_dc_2x2(&mut br, &mut out),
            Err(Error::BadBitWidth(_))
        ));
    }

    #[test]
    fn chroma_dc_block_fills_without_end_symbol() {
        let items: Vec<(u32, u32)> = (0..4).map(|_| ue(1)).collect();
        let bytes = pack(&items);
        let mut br = BitReader::new(&bytes);
        let mut out = [0i32; 4];
        let pos = decode_chroma_dc_2x2(&mut br, &mut out).expect("decode");
        assert_eq!(pos, 4);
        assert_eq!(out, [1i32; 4]);
        // Four 3-bit codewords were consumed and nothing else — no
        // end-of-block symbol after the fill.
        assert_eq!(br.bits_consumed(), 12, "no end-of-block symbol consumed");
    }

    #[test]
    fn truncated_input_propagates() {
        let mut br = BitReader::new(&[]);
        let mut out = [0i32; 16];
        assert!(matches!(
            decode_residual_4x4_normal(&mut br, &NORMAL_ZIGZAG_4X4_SCAN, 0, &mut out),
            Err(Error::Truncated)
        ));
    }
}
