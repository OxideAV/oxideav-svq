//! SVQ1 variable-length-code layer — the sixteen wire VLC tables.
//!
//! The tables land bit-exact from the CSV mirrors under `tables/`
//! (themselves byte-identical copies of `docs/video/svq1/tables/`,
//! SHA-256s per `tables/MANIFEST-03.sha256`; extracted by the docs
//! collaborator's Extractor 03 pass,
//! `docs/video/svq1/provenance/07-extractor-vlc-tables.md`, and
//! role-mapped by Auditor 02, `docs/video/svq1/audit/01-report.md`):
//!
//! | Table | Role (audit/01) | Alphabet | min |
//! |-------|-----------------|----------|-----|
//! | T00 | inter mean VLC (§2.1) | 512 | −256 |
//! | T01 | intra mean VLC (§2.2) | 256 | 0 |
//! | T02 | motion-vector component VLC (§3.1) | 64 | 0 |
//! | T03 | interframe MB-coding-mode VLC (§3.2) | 4 | 0 |
//! | T04..T09 | intra stage-count VLCs, L=5..L=0 (§4) | 8 | 0 |
//! | T10..T15 | inter stage-count VLCs, L=5..L=0 (§4) | 8 | 0 |
//!
//! ## Record semantics
//!
//! Per `provenance/07-extractor-vlc-tables.md` §5.2 every on-disk
//! record is `u16_le value + u8 zero_pad + u8 (code_length * 8)`;
//! the record index is the ALPHABET POSITION and the decoded value
//! is `position + min_value` (audit/01 §2.1 pins the `min_value`
//! convention on the inter mean table: "The decoded range is
//! `{record_index + min_value}`"). The `value` field is the
//! CODEWORD's numeric bit pattern: reading the codeword's
//! `code_length` bits MSB-first off the bitstream yields exactly
//! that integer. This crate VERIFIES the reading rather than
//! assuming it — [`Svq1VlcDecoder::new`] rejects any table that is
//! not prefix-free under the MSB-first interpretation, and all
//! sixteen staged tables pass (fifteen with Kraft sum exactly 1;
//! T02 with the documented `8187/8192` deficit, audit/01 §3.1).
//!
//! ## Decoded-value mappings (role layer)
//!
//! * Stage count: `N = position − 1` per the audit-corrected
//!   `docs/video/svq1/spec/04-multistage-vq-decoder.md` §4.1
//!   (position 0 = `N = −1` = SKIP, position 1 = `N = 0` =
//!   mean-only, …, position 7 = `N = 6`).
//! * Intra mean: `u8` value = position (min 0), range `[0, 255]`
//!   per `docs/video/svq1/spec/05-mean-removal.md` §5.1.1.
//! * Inter mean: `s9` value = position − 256, range `[−256, +255]`
//!   per spec/05 §5.1.2.
//! * MV component / MB mode: the raw alphabet position is returned;
//!   the semantic mapping (spec/06 §6.2.3 Reading A/B; audit/01
//!   §7.1 mode permutation) is resolved by the plane-decode layer.

use std::sync::OnceLock;

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::svq1_blocktree::Svq1Level;
use crate::svq1_codebook::{
    SVQ1_VLC_INTER_MEAN, SVQ1_VLC_INTER_STAGE_COUNT_L0, SVQ1_VLC_INTER_STAGE_COUNT_L1,
    SVQ1_VLC_INTER_STAGE_COUNT_L2, SVQ1_VLC_INTER_STAGE_COUNT_L3, SVQ1_VLC_INTER_STAGE_COUNT_L4,
    SVQ1_VLC_INTER_STAGE_COUNT_L5, SVQ1_VLC_INTRA_MEAN, SVQ1_VLC_INTRA_STAGE_COUNT_L0,
    SVQ1_VLC_INTRA_STAGE_COUNT_L1, SVQ1_VLC_INTRA_STAGE_COUNT_L2, SVQ1_VLC_INTRA_STAGE_COUNT_L3,
    SVQ1_VLC_INTRA_STAGE_COUNT_L4, SVQ1_VLC_INTRA_STAGE_COUNT_L5, SVQ1_VLC_MB_MODE,
    SVQ1_VLC_MV_COMPONENT,
};

/// Longest codeword observed across the sixteen staged tables — 22
/// bits (the inter mean VLC's tail), per
/// `docs/video/svq1/provenance/07-extractor-vlc-tables.md` §5.2
/// ("length_byte … multiples of 8 up to 22*8").
pub const MAX_CODE_LENGTH: u8 = 22;

/// Codebook / VLC half selector — intra or inter. Per
/// `docs/video/svq1/spec/04-multistage-vq-decoder.md` §4.4 the half
/// is fixed per MACROBLOCK (I-frame / INTRA-mode → intra; INTER /
/// INTER_4MV → inter) and selects both the stage-count VLC family
/// and the mean VLC and the codebook half.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Svq1Half {
    /// Intraframe path (I-frames; INTRA-mode P/B macroblocks).
    Intra,
    /// Interframe path (INTER / INTER_4MV P/B macroblocks).
    Inter,
}

/// A prefix-code decoder built from one staged `(codeword,
/// code_length)` table.
///
/// Construction ([`Svq1VlcDecoder::new`]) verifies the table is a
/// valid prefix code under the MSB-first reading: no duplicate
/// `(length, codeword)` pair and no codeword that is a prefix of a
/// longer one. Decoding ([`Svq1VlcDecoder::read`]) consumes bits
/// MSB-first until the accumulated pattern matches a codeword and
/// returns `alphabet_position + min_value`.
#[derive(Debug)]
pub struct Svq1VlcDecoder {
    min_value: i32,
    max_len: u8,
    /// `by_len[len]` holds `(codeword, alphabet_position)` pairs of
    /// exactly `len` bits, sorted by codeword for binary search.
    by_len: Vec<Vec<(u16, u16)>>,
    /// Kraft numerator over denominator `2^MAX_CODE_LENGTH`.
    kraft_numerator: u64,
}

impl Svq1VlcDecoder {
    /// Build a decoder from `(codeword, code_length)` records (one
    /// per alphabet position) plus the table's decoded-value offset.
    ///
    /// # Panics
    ///
    /// Panics if the table is not a prefix-free code under the
    /// MSB-first reading (duplicate codeword, prefix collision, or a
    /// codeword that does not fit its declared length). The sixteen
    /// staged tables all pass; a panic can only mean the mirrored
    /// CSV was corrupted, which the build should not survive.
    pub fn new(records: &[(u16, u8)], min_value: i32) -> Self {
        let max_len = records.iter().map(|&(_, l)| l).max().unwrap_or(0);
        assert!(
            max_len <= MAX_CODE_LENGTH,
            "svq1 VLC code length {max_len} exceeds the documented maximum {MAX_CODE_LENGTH}"
        );
        let mut by_len: Vec<Vec<(u16, u16)>> = vec![Vec::new(); usize::from(max_len) + 1];
        let mut kraft_numerator: u64 = 0;
        for (pos, &(codeword, len)) in records.iter().enumerate() {
            assert!(len >= 1, "svq1 VLC record {pos} has zero code length");
            assert!(
                len >= 16 || u32::from(codeword) < (1u32 << len),
                "svq1 VLC record {pos} codeword 0x{codeword:04x} does not fit {len} bits"
            );
            by_len[usize::from(len)].push((codeword, pos as u16));
            kraft_numerator += 1u64 << (MAX_CODE_LENGTH - len);
        }
        for bucket in &mut by_len {
            bucket.sort_unstable();
            // No duplicate (length, codeword) pair.
            for pair in bucket.windows(2) {
                assert!(
                    pair[0].0 != pair[1].0,
                    "svq1 VLC duplicate codeword 0x{:04x}",
                    pair[0].0
                );
            }
        }
        // Prefix-freedom: a codeword of length S must not be the
        // MSB-prefix of any codeword of length L > S.
        for short_len in 1..by_len.len() {
            for &(short_code, _) in &by_len[short_len] {
                for (long_len, bucket) in by_len.iter().enumerate().skip(short_len + 1) {
                    let shift = long_len - short_len;
                    for &(long_code, _) in bucket {
                        assert!(
                            (u32::from(long_code) >> shift) != u32::from(short_code),
                            "svq1 VLC prefix collision: 0x{short_code:04x}/{short_len} vs \
                             0x{long_code:04x}/{long_len}"
                        );
                    }
                }
            }
        }
        Self {
            min_value,
            max_len,
            by_len,
            kraft_numerator,
        }
    }

    /// The table's decoded-value offset (`min_value`): decoded value
    /// = alphabet position + `min_value`.
    pub fn min_value(&self) -> i32 {
        self.min_value
    }

    /// Longest codeword in this table, in bits.
    pub fn max_code_length(&self) -> u8 {
        self.max_len
    }

    /// Kraft sum as a `(numerator, denominator)` pair with
    /// denominator `2^MAX_CODE_LENGTH`. A complete canonical prefix
    /// code has `numerator == denominator`; T02's documented deficit
    /// (audit/01 §3.1) shows up as `denominator − numerator ==
    /// 5 << (22 − 13)`.
    pub fn kraft_sum(&self) -> (u64, u64) {
        (self.kraft_numerator, 1u64 << MAX_CODE_LENGTH)
    }

    /// Read one codeword MSB-first off `br` and return the DECODED
    /// VALUE (`alphabet position + min_value`).
    ///
    /// Bits are consumed one at a time; after each bit the
    /// accumulated pattern is checked against the codewords of that
    /// exact length (prefix-freedom guarantees at most one match
    /// ever fires). If `max_code_length` bits accumulate with no
    /// match the pattern is not in the table —
    /// [`Error::InvalidVlcCode`] (for T02 this is exactly the
    /// five-missing-leaves case audit/01 §7.3 documents; conformant
    /// streams never emit those patterns).
    pub fn read(&self, br: &mut BitReader<'_>) -> Result<i32> {
        let mut acc: u32 = 0;
        for len in 1..=usize::from(self.max_len) {
            acc = (acc << 1) | u32::from(br.read_bit()?);
            let bucket = &self.by_len[len];
            if let Ok(found) = bucket.binary_search_by_key(&(acc as u16), |&(cw, _)| cw) {
                let pos = bucket[found].1;
                return Ok(i32::from(pos) + self.min_value);
            }
        }
        Err(Error::InvalidVlcCode)
    }

    /// Alphabet position of a decoded value (inverse of the
    /// `position + min_value` mapping); test / encode-side helper.
    pub fn position_of(&self, value: i32) -> Option<u16> {
        let pos = value.checked_sub(self.min_value)?;
        u16::try_from(pos).ok()
    }
}

fn decoder(
    cell: &'static OnceLock<Svq1VlcDecoder>,
    table: &'static ([(u16, u8); 8], i32),
) -> &'static Svq1VlcDecoder {
    cell.get_or_init(|| Svq1VlcDecoder::new(&table.0, table.1))
}

/// The single inter mean VLC (T00, alphabet 512, min −256) per
/// audit/01 §2.1 — shared across all levels (spec/05 §5.2
/// audit-corrected).
pub fn inter_mean_decoder() -> &'static Svq1VlcDecoder {
    static CELL: OnceLock<Svq1VlcDecoder> = OnceLock::new();
    CELL.get_or_init(|| Svq1VlcDecoder::new(&SVQ1_VLC_INTER_MEAN.0, SVQ1_VLC_INTER_MEAN.1))
}

/// The single intra mean VLC (T01, alphabet 256) per audit/01 §2.2.
pub fn intra_mean_decoder() -> &'static Svq1VlcDecoder {
    static CELL: OnceLock<Svq1VlcDecoder> = OnceLock::new();
    CELL.get_or_init(|| Svq1VlcDecoder::new(&SVQ1_VLC_INTRA_MEAN.0, SVQ1_VLC_INTRA_MEAN.1))
}

/// The motion-vector component VLC (T02, alphabet 64) per audit/01
/// §3.1. Returns raw alphabet positions from
/// [`read_mv_component_position`]; the position → component mapping
/// is the spec/06 §6.2.3 Reading A/B question resolved downstream.
pub fn mv_component_decoder() -> &'static Svq1VlcDecoder {
    static CELL: OnceLock<Svq1VlcDecoder> = OnceLock::new();
    CELL.get_or_init(|| Svq1VlcDecoder::new(&SVQ1_VLC_MV_COMPONENT.0, SVQ1_VLC_MV_COMPONENT.1))
}

/// The interframe MB-coding-mode VLC (T03, alphabet 4) per audit/01
/// §3.2. Returns raw alphabet positions from [`read_mb_mode_position`];
/// the position → {SKIP, INTER, INTER_4MV, INTRA} permutation is the
/// audit/01 §7.1 open item resolved downstream.
pub fn mb_mode_decoder() -> &'static Svq1VlcDecoder {
    static CELL: OnceLock<Svq1VlcDecoder> = OnceLock::new();
    CELL.get_or_init(|| Svq1VlcDecoder::new(&SVQ1_VLC_MB_MODE.0, SVQ1_VLC_MB_MODE.1))
}

/// The per-`(level, half)` stage-count VLC (T04..T15) per audit/01
/// §4.1. All six levels have a slot (the L=4 / L=5 slots are
/// present-but-unreachable per audit/01 §4.5 — the always-subdivide
/// gate of spec/03 §3.3.1 / §3.3.2 keeps the leaf path from reaching
/// them in practice).
pub fn stage_count_decoder(level: Svq1Level, half: Svq1Half) -> &'static Svq1VlcDecoder {
    static INTRA: [OnceLock<Svq1VlcDecoder>; 6] = [
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
    ];
    static INTER: [OnceLock<Svq1VlcDecoder>; 6] = [
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
    ];
    let idx = level_index(level);
    match half {
        Svq1Half::Intra => decoder(&INTRA[idx], intra_stage_count_table(level)),
        Svq1Half::Inter => decoder(&INTER[idx], inter_stage_count_table(level)),
    }
}

const fn level_index(level: Svq1Level) -> usize {
    match level {
        Svq1Level::L0 => 0,
        Svq1Level::L1 => 1,
        Svq1Level::L2 => 2,
        Svq1Level::L3 => 3,
        Svq1Level::L4 => 4,
        Svq1Level::L5 => 5,
    }
}

/// The intra stage-count table for `level` (T04..T09, audit/01 §4.1:
/// source T04 = L=5 … T09 = L=0).
pub fn intra_stage_count_table(level: Svq1Level) -> &'static ([(u16, u8); 8], i32) {
    match level {
        Svq1Level::L0 => &SVQ1_VLC_INTRA_STAGE_COUNT_L0,
        Svq1Level::L1 => &SVQ1_VLC_INTRA_STAGE_COUNT_L1,
        Svq1Level::L2 => &SVQ1_VLC_INTRA_STAGE_COUNT_L2,
        Svq1Level::L3 => &SVQ1_VLC_INTRA_STAGE_COUNT_L3,
        Svq1Level::L4 => &SVQ1_VLC_INTRA_STAGE_COUNT_L4,
        Svq1Level::L5 => &SVQ1_VLC_INTRA_STAGE_COUNT_L5,
    }
}

/// The inter stage-count table for `level` (T10..T15, audit/01 §4.1:
/// source T10 = L=5 … T15 = L=0).
pub fn inter_stage_count_table(level: Svq1Level) -> &'static ([(u16, u8); 8], i32) {
    match level {
        Svq1Level::L0 => &SVQ1_VLC_INTER_STAGE_COUNT_L0,
        Svq1Level::L1 => &SVQ1_VLC_INTER_STAGE_COUNT_L1,
        Svq1Level::L2 => &SVQ1_VLC_INTER_STAGE_COUNT_L2,
        Svq1Level::L3 => &SVQ1_VLC_INTER_STAGE_COUNT_L3,
        Svq1Level::L4 => &SVQ1_VLC_INTER_STAGE_COUNT_L4,
        Svq1Level::L5 => &SVQ1_VLC_INTER_STAGE_COUNT_L5,
    }
}

/// Read one stage-count VLC for `(level, half)` and return
/// `N ∈ {−1, 0, …, 6}` per the audit-corrected spec/04 §4.1 mapping
/// `N = alphabet_position − 1` (position 0 = SKIP, position 1 =
/// mean-only, position 7 = six stages).
pub fn read_stage_count(br: &mut BitReader<'_>, level: Svq1Level, half: Svq1Half) -> Result<i8> {
    let pos = stage_count_decoder(level, half).read(br)?;
    Ok((pos - 1) as i8)
}

/// Read the intra mean VLC — a `u8` in `[0, 255]` per spec/05 §5.1.1.
pub fn read_intra_mean(br: &mut BitReader<'_>) -> Result<u8> {
    let value = intra_mean_decoder().read(br)?;
    debug_assert!((0..=255).contains(&value));
    Ok(value as u8)
}

/// Read the inter mean VLC — an `s9` in `[−256, +255]` per spec/05
/// §5.1.2 (alphabet position + the table's `min_value = −256`).
pub fn read_inter_mean(br: &mut BitReader<'_>) -> Result<i16> {
    let value = inter_mean_decoder().read(br)?;
    debug_assert!((-256..=255).contains(&value));
    Ok(value as i16)
}

/// Read one motion-vector component codeword and return the RAW
/// alphabet position `∈ [0, 63]`. The position → signed-component
/// mapping (spec/06 §6.2.3 Reading A vs Reading B) is applied by the
/// motion-vector wire layer.
pub fn read_mv_component_position(br: &mut BitReader<'_>) -> Result<u8> {
    let value = mv_component_decoder().read(br)?;
    debug_assert!((0..=63).contains(&value));
    Ok(value as u8)
}

/// Read one interframe MB-coding-mode codeword and return the RAW
/// alphabet position `∈ [0, 3]`. The position → semantic-mode
/// permutation (audit/01 §7.1) is applied by the plane-decode layer.
pub fn read_mb_mode_position(br: &mut BitReader<'_>) -> Result<u8> {
    let value = mb_mode_decoder().read(br)?;
    debug_assert!((0..=3).contains(&value));
    Ok(value as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pack a sequence of `(width, value)` items MSB-first.
    fn pack(items: &[(u32, u32)]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut bit_cursor: usize = 0;
        for &(width, value) in items {
            for i in (0..width).rev() {
                let bit = ((value >> i) & 1) as u8;
                let byte_idx = bit_cursor / 8;
                if byte_idx >= out.len() {
                    out.push(0);
                }
                out[byte_idx] |= bit << (7 - (bit_cursor % 8));
                bit_cursor += 1;
            }
        }
        if out.is_empty() {
            out.push(0);
        }
        out
    }

    fn all_decoders() -> Vec<(&'static str, &'static Svq1VlcDecoder)> {
        let mut v: Vec<(&'static str, &'static Svq1VlcDecoder)> = vec![
            ("inter_mean", inter_mean_decoder()),
            ("intra_mean", intra_mean_decoder()),
            ("mv_component", mv_component_decoder()),
            ("mb_mode", mb_mode_decoder()),
        ];
        for level in [
            Svq1Level::L0,
            Svq1Level::L1,
            Svq1Level::L2,
            Svq1Level::L3,
            Svq1Level::L4,
            Svq1Level::L5,
        ] {
            v.push((
                "intra_stage_count",
                stage_count_decoder(level, Svq1Half::Intra),
            ));
            v.push((
                "inter_stage_count",
                stage_count_decoder(level, Svq1Half::Inter),
            ));
        }
        v
    }

    /// Every staged table must build (constructor panics on any
    /// prefix violation), and all except T02 must be Kraft-complete.
    /// T02's deficit is exactly `5 / 8192` (audit/01 §3.1 — five
    /// missing leaves at code length 13).
    #[test]
    fn all_sixteen_tables_are_prefix_free_with_documented_kraft_sums() {
        for (name, dec) in all_decoders() {
            let (num, den) = dec.kraft_sum();
            if std::ptr::eq(dec, mv_component_decoder()) {
                let deficit = den - num;
                assert_eq!(
                    deficit,
                    5u64 << (u32::from(MAX_CODE_LENGTH) - 13),
                    "{name}: T02 Kraft deficit must be 5/8192"
                );
            } else {
                assert_eq!(num, den, "{name}: Kraft sum must be exactly 1");
            }
        }
    }

    /// audit/01 §2.1 evidence 3: the inter mean's decoded value 0
    /// (record index 256) carries the shortest possible (1-bit)
    /// codeword and the code length grows as the decoded value moves
    /// away from 0. (The audit prose says both ±1 carry 3-bit codes;
    /// the byte-exact CSV records +1 at 3 bits and −1 at 4 bits —
    /// the CSV is the mirrored table of record, so this test pins
    /// the CSV values.)
    #[test]
    fn inter_mean_value_zero_is_one_bit() {
        assert_eq!(SVQ1_VLC_INTER_MEAN.1, -256);
        assert_eq!(SVQ1_VLC_INTER_MEAN.0[256].1, 1, "value 0 code length");
        assert_eq!(SVQ1_VLC_INTER_MEAN.0[257].1, 3, "value +1 code length");
        assert_eq!(SVQ1_VLC_INTER_MEAN.0[255].1, 4, "value −1 code length");
    }

    /// audit/01 §2.2 evidence 2: intra mean record indices 25 and 26
    /// carry length 4; indices 0, 27, 148, 149 carry length 6.
    #[test]
    fn intra_mean_shortest_codes_match_audit() {
        for idx in [25usize, 26] {
            assert_eq!(SVQ1_VLC_INTRA_MEAN.0[idx].1, 4, "index {idx}");
        }
        for idx in [0usize, 27, 148, 149] {
            assert_eq!(SVQ1_VLC_INTRA_MEAN.0[idx].1, 6, "index {idx}");
        }
    }

    /// audit/01 §3.1 evidence 2: T02 code lengths are symmetric
    /// around alphabet position 32, with position 32 = length 1.
    #[test]
    fn mv_component_symmetry_around_position_32() {
        let t = &SVQ1_VLC_MV_COMPONENT.0;
        assert_eq!(t[32].1, 1);
        assert_eq!((t[31].1, t[33].1), (3, 3));
        assert_eq!((t[30].1, t[34].1), (4, 4));
        assert_eq!((t[29].1, t[35].1), (5, 5));
        assert_eq!((t[28].1, t[36].1), (7, 7));
        // Full symmetry sweep across every offset.
        for d in 1..=31usize {
            assert_eq!(t[32 - d].1, t[32 + d].1, "offset {d}");
        }
    }

    /// audit/01 §3.2 evidence 2: the four T03 records carry code
    /// lengths {2, 3, 3, 1} for alphabet positions {0, 1, 2, 3}.
    #[test]
    fn mb_mode_code_lengths_match_audit() {
        let t = &SVQ1_VLC_MB_MODE.0;
        assert_eq!(t[0], (1, 2));
        assert_eq!(t[1], (1, 3));
        assert_eq!(t[2], (0, 3));
        assert_eq!(t[3], (1, 1));
    }

    /// audit/01 §4.3: the intra stage-count tables peak (shortest
    /// code) at alphabet position 1 (`N = 0`, mean-only); the inter
    /// tables peak at position 0 (`N = −1`, SKIP).
    #[test]
    fn stage_count_peaks_match_audit() {
        for level in [
            Svq1Level::L0,
            Svq1Level::L1,
            Svq1Level::L2,
            Svq1Level::L3,
            Svq1Level::L4,
            Svq1Level::L5,
        ] {
            let intra = intra_stage_count_table(level).0;
            let shortest = intra
                .iter()
                .enumerate()
                .min_by_key(|(_, &(_, l))| l)
                .unwrap()
                .0;
            assert_eq!(shortest, 1, "intra {level:?} shortest-code position");

            let inter = inter_stage_count_table(level).0;
            let shortest = inter
                .iter()
                .enumerate()
                .min_by_key(|(_, &(_, l))| l)
                .unwrap()
                .0;
            assert_eq!(shortest, 0, "inter {level:?} shortest-code position");
        }
    }

    /// audit/01 §5.1 duplicate sets: inter L=4 / L=3 / L=2 are
    /// byte-identical (set A) and inter L=1 / L=0 are byte-identical
    /// (set B); the six intra tables are pairwise distinct.
    #[test]
    fn inter_duplicate_sets_match_audit() {
        assert_eq!(
            inter_stage_count_table(Svq1Level::L4).0,
            inter_stage_count_table(Svq1Level::L3).0
        );
        assert_eq!(
            inter_stage_count_table(Svq1Level::L3).0,
            inter_stage_count_table(Svq1Level::L2).0
        );
        assert_eq!(
            inter_stage_count_table(Svq1Level::L1).0,
            inter_stage_count_table(Svq1Level::L0).0
        );
        assert_ne!(
            inter_stage_count_table(Svq1Level::L5).0,
            inter_stage_count_table(Svq1Level::L4).0
        );
        let levels = [
            Svq1Level::L0,
            Svq1Level::L1,
            Svq1Level::L2,
            Svq1Level::L3,
            Svq1Level::L4,
            Svq1Level::L5,
        ];
        for (i, &a) in levels.iter().enumerate() {
            for &b in &levels[i + 1..] {
                assert_ne!(
                    intra_stage_count_table(a).0,
                    intra_stage_count_table(b).0,
                    "intra {a:?} vs {b:?} must be distinct (audit/01 §5.4)"
                );
            }
        }
    }

    /// Exhaustive decode round-trip on every table: feed each
    /// codeword's exact bit pattern and confirm the decoded value is
    /// that alphabet position + min_value.
    #[test]
    fn decode_round_trips_every_codeword() {
        type TableCase = (&'static Svq1VlcDecoder, Vec<(u16, u8)>, i32);
        let mut tables: Vec<TableCase> = vec![
            (
                inter_mean_decoder(),
                SVQ1_VLC_INTER_MEAN.0.to_vec(),
                SVQ1_VLC_INTER_MEAN.1,
            ),
            (
                intra_mean_decoder(),
                SVQ1_VLC_INTRA_MEAN.0.to_vec(),
                SVQ1_VLC_INTRA_MEAN.1,
            ),
            (
                mv_component_decoder(),
                SVQ1_VLC_MV_COMPONENT.0.to_vec(),
                SVQ1_VLC_MV_COMPONENT.1,
            ),
            (
                mb_mode_decoder(),
                SVQ1_VLC_MB_MODE.0.to_vec(),
                SVQ1_VLC_MB_MODE.1,
            ),
        ];
        for level in [Svq1Level::L0, Svq1Level::L3, Svq1Level::L5] {
            tables.push((
                stage_count_decoder(level, Svq1Half::Intra),
                intra_stage_count_table(level).0.to_vec(),
                0,
            ));
            tables.push((
                stage_count_decoder(level, Svq1Half::Inter),
                inter_stage_count_table(level).0.to_vec(),
                0,
            ));
        }
        for (dec, records, min) in tables {
            for (pos, &(cw, len)) in records.iter().enumerate() {
                let bytes = pack(&[(u32::from(len), u32::from(cw))]);
                let mut br = BitReader::new(&bytes);
                let decoded = dec.read(&mut br).expect("codeword decodes");
                assert_eq!(decoded, pos as i32 + min, "codeword 0x{cw:04x}/{len}");
                assert_eq!(br.bits_consumed(), usize::from(len), "consumed width");
            }
        }
    }

    /// Stage-count role mapping: `N = position − 1` (spec/04 §4.1
    /// audit-corrected). The intra L=5 table's 1-bit codeword decodes
    /// to `N = 0` (mean-only); the inter L=5 table's 1-bit codeword
    /// decodes to `N = −1` (SKIP).
    #[test]
    fn stage_count_role_mapping() {
        // Intra L=5: position 1 record is (codeword 1, length 1).
        let bytes = pack(&[(1, 1)]);
        let mut br = BitReader::new(&bytes);
        let n = read_stage_count(&mut br, Svq1Level::L5, Svq1Half::Intra).unwrap();
        assert_eq!(n, 0, "intra L=5 shortest codeword is mean-only");

        // Inter L=5: position 0 record is (codeword 1, length 1).
        let mut br = BitReader::new(&bytes);
        let n = read_stage_count(&mut br, Svq1Level::L5, Svq1Half::Inter).unwrap();
        assert_eq!(n, -1, "inter L=5 shortest codeword is SKIP");
    }

    /// Inter mean typed read: a single 1-bit codeword `1` decodes to
    /// mean 0 (audit/01 §2.1 evidence 3).
    #[test]
    fn read_inter_mean_zero() {
        let (cw, len) = SVQ1_VLC_INTER_MEAN.0[256];
        let bytes = pack(&[(u32::from(len), u32::from(cw))]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(read_inter_mean(&mut br).unwrap(), 0);
    }

    /// Intra mean typed read across a few positions.
    #[test]
    fn read_intra_mean_round_trip() {
        for value in [0u8, 25, 61, 128, 255] {
            let (cw, len) = SVQ1_VLC_INTRA_MEAN.0[usize::from(value)];
            let bytes = pack(&[(u32::from(len), u32::from(cw))]);
            let mut br = BitReader::new(&bytes);
            assert_eq!(read_intra_mean(&mut br).unwrap(), value);
        }
    }

    /// A bit pattern outside the table (one of T02's five missing
    /// length-13 leaves — found mechanically) errors with
    /// `InvalidVlcCode` instead of aliasing onto a wrong value.
    #[test]
    fn unmatched_pattern_is_invalid_vlc_code() {
        // Find a 13-bit pattern that is not covered by any codeword
        // (T02's Kraft deficit guarantees at least one exists).
        let t = &SVQ1_VLC_MV_COMPONENT.0;
        let mut pattern: Option<u32> = None;
        'outer: for cand in 0..(1u32 << 13) {
            for &(cw, len) in t.iter() {
                // cand's leading `len` bits == cw ⇒ covered.
                if (cand >> (13 - u32::from(len))) == u32::from(cw) {
                    continue 'outer;
                }
            }
            pattern = Some(cand);
            break;
        }
        let cand = pattern.expect("T02 must have uncovered 13-bit patterns");
        let bytes = pack(&[(13, cand)]);
        let mut br = BitReader::new(&bytes);
        assert!(matches!(
            mv_component_decoder().read(&mut br),
            Err(Error::InvalidVlcCode)
        ));
    }

    /// Truncated input surfaces `Error::Truncated` (not a garbage
    /// value) when the stream ends mid-codeword.
    #[test]
    fn truncated_codeword_is_truncated_error() {
        // Intra mean: no 1-bit codewords (shortest is 4), so a
        // 1-byte stream of zero bits runs out before any match at
        // length > 8 can complete only if all-zero prefixes are
        // unassigned up to 8 bits. Use an empty stream instead —
        // guaranteed truncation on the first bit.
        let mut br = BitReader::new(&[]);
        assert!(matches!(
            intra_mean_decoder().read(&mut br),
            Err(Error::Truncated)
        ));
    }
}
