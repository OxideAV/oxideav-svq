//! SVQ1 per-stage codebook-index field reader (`4 × N` bits, raw).
//!
//! ## Provenance
//!
//! Round 242 implements the per-stage codebook-index field reads the
//! SVQ1 spec defines in `docs/video/svq1/spec/04-multistage-vq-decoder.md`
//! §4.2 ("Per-stage codebook-index field reads"). After the per-leaf
//! stage-count VLC has decoded `N ∈ 1..=6`, and after the per-leaf
//! mean VLC has been consumed, the bit-reader cursor sits at the
//! start of a fixed-width `4 × N`-bit run carrying the codebook
//! vector indices for stage-1 through stage-`N`.
//!
//! The wiki source pins the same field arrangement:
//!
//! > `wiki/Sorenson_Video_1.wiki` §"Decoding Intraframe Plane Data"
//! > line *"fetch the next (stages * 4) bits from the bitstream;
//! > these specify the codebooks to use in reconstructing the image;
//! > the first 4 bits specify the first codebook to use, the next 4
//! > bits specify the second codebook, all the way up to a sixth
//! > codebooks for 6 possible stages"*.
//!
//! Round 242's scope is the bitstream-side index reader alone — it
//! does not perform the codebook lookup itself (that needs the per-
//! `(level, half, stage)` payload-offset arithmetic still tracked
//! against the L=0..L=3 payload's intra-vs-inter / stage-vs-level
//! interleave open item in
//! [`crate::svq1_codebook`]). The output is `N` raw unsigned 4-bit
//! values in `0..=15`, in stage-ascending order.
//!
//! ## Spec invariants surfaced by this module
//!
//! * Per-stage width is **exactly 4 bits**, no padding (§4.2 table
//!   row "Width : 4 bits", §4.2.1 line *"No padding: stage `k+1`'s
//!   4-bit field begins at the bit immediately after stage `k`'s
//!   field's last bit"*).
//! * Stage ordering is **fixed** — stage-1 is always first, stage-`N`
//!   last (§4.2.1 line *"No reordering: stage-1 index is ALWAYS
//!   first; stage-2 is ALWAYS second; etc. There is no permutation
//!   field"*).
//! * Each 4-bit field is a **raw** unsigned integer in `0..=15`, not
//!   a VLC codeword (§4.2.1 line *"Raw binary: each 4-bit field is a
//!   raw unsigned integer, NOT a VLC codeword"*).
//! * The maximum stage count is **six** (§4.2 table and
//!   [`crate::svq1_codebook::SVQ1_STAGES_PER_LEVEL`]); the spec gates
//!   higher values via the stage-count VLC alphabet `{-1, 0..=6}`
//!   (§4.1.1) and the L≥4-quantise validity gate (§4.1.2).
//!
//! Both endpoints (`N = 0` for the mean-only leaf path and `N = -1`
//! for the SKIP path) consume **zero** stage-index bits — the
//! module's [`read_stage_indices`] returns an empty `IndexBuffer`
//! at `N = 0` and the call is undefined at `N = -1` (the caller is
//! expected to short-circuit SKIP before reaching the index reader,
//! per §4.5.5).
//!
//! ## Wire-up plan (out of scope this round)
//!
//! The codebook lookup that consumes these indices is the §4.3
//! step: `(level, half, stage, vec_idx) → V_L signed bytes`. That
//! step needs the L=0..L=3 payload's intra-vs-inter / stage-vs-level
//! interleave pinned in `docs/video/svq1/` (currently the
//! [`crate::svq1_codebook`] module's "Open work" item). Once that
//! lands, the per-leaf decoder will call this module's
//! [`read_stage_indices`] and feed the returned `IndexBuffer` into
//! the codebook-offset arithmetic.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};

/// Width of one codebook-index field in bits.
///
/// Per spec/04 §4.2 each per-stage field is `4` bits wide. Each
/// 4-bit field encodes the unsigned `vec_idx ∈ 0..=15` that selects
/// one of the sixteen vectors stored in the corresponding
/// `(level, half, stage)` codebook page (per spec/14 §14.4 / §14.5).
pub const BITS_PER_INDEX: u32 = 4;

/// Maximum number of stages per leaf — six.
///
/// Per spec/04 §4.1 (stage-count alphabet) and spec/14 §14.1 / §14.4,
/// the multistage VQ stack is at most six stages deep. The stage-count
/// VLC decodes to a value in `{-1, 0, 1, 2, 3, 4, 5, 6}`; this
/// constant mirrors the upper bound of that range. See
/// [`crate::svq1_codebook::SVQ1_STAGES_PER_LEVEL`] for the
/// codebook-side equivalent.
pub const MAX_STAGES_PER_LEAF: usize = 6;

/// Highest `vec_idx` a 4-bit raw index field can encode.
///
/// `(1 << BITS_PER_INDEX) - 1 = 15`, matching spec/14 §14.4 "16
/// vectors per stage" (entries `0..=15`). Surfaced as a `const` so
/// callers can range-check against the same upper bound the reader
/// is guaranteed to honour.
pub const MAX_VEC_IDX: u8 = (1u8 << BITS_PER_INDEX) - 1;

/// Compute the total number of bits the per-stage index run consumes
/// for `N` stages (`N ∈ 0..=MAX_STAGES_PER_LEAF`).
///
/// Returns `4 × N`. For `N = 0` (mean-only leaf, spec/04 §4.5.4)
/// the run is empty and the function returns `0`. For
/// `N > MAX_STAGES_PER_LEAF` returns `None` — a well-formed
/// bitstream cannot reach this branch (the stage-count VLC alphabet
/// excludes it per spec/04 §4.1.1).
pub const fn bits_for_n_stages(n_stages: usize) -> Option<usize> {
    if n_stages > MAX_STAGES_PER_LEAF {
        return None;
    }
    Some(n_stages * BITS_PER_INDEX as usize)
}

/// Fixed-capacity buffer of decoded `vec_idx` values, in
/// stage-ascending order.
///
/// `len` records the number of populated entries (the `N` parameter
/// passed to [`read_stage_indices`]); positions `len..MAX_STAGES_PER_LEAF`
/// are left at the `0` initialisation value and MUST NOT be read by
/// callers — the [`IndexBuffer::indices`] accessor returns a slice
/// limited to the populated range.
///
/// The buffer is intentionally a fixed-size array rather than a
/// `Vec` to keep [`read_stage_indices`] allocation-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexBuffer {
    indices: [u8; MAX_STAGES_PER_LEAF],
    len: u8,
}

impl IndexBuffer {
    /// Empty buffer (no indices populated). Used internally as the
    /// `N = 0` return value and by tests / construction helpers.
    pub const EMPTY: Self = Self {
        indices: [0u8; MAX_STAGES_PER_LEAF],
        len: 0,
    };

    /// Number of populated indices. Equal to the `N` parameter the
    /// reader was called with.
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// `true` when no indices are populated (`N = 0` — the
    /// mean-only leaf path per spec/04 §4.5.4).
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Borrowed slice over the populated `vec_idx` values, in
    /// stage-ascending order (stage-1 first, stage-`N` last per
    /// spec/04 §4.2.1).
    pub fn indices(&self) -> &[u8] {
        &self.indices[..self.len as usize]
    }

    /// Return the `vec_idx` for `stage_one_based ∈ 1..=N`, or `None`
    /// if the requested stage is past the populated count.
    ///
    /// Uses **one-based** stage numbering to match the spec's
    /// stage-1 / stage-2 / … / stage-`N` terminology
    /// (spec/04 §4.2 column "Stage").
    pub fn get(&self, stage_one_based: usize) -> Option<u8> {
        if stage_one_based == 0 || stage_one_based > self.len as usize {
            return None;
        }
        Some(self.indices[stage_one_based - 1])
    }
}

/// Read `n_stages` consecutive 4-bit codebook-index fields from
/// `reader` and return them as an [`IndexBuffer`] in stage-ascending
/// order.
///
/// Per spec/04 §4.2:
///
/// * `n_stages` MUST be in `0..=MAX_STAGES_PER_LEAF`. For
///   `n_stages > MAX_STAGES_PER_LEAF` the function returns
///   [`Error::BadBitWidth`] carrying the offending stage count — the
///   spec/04 §4.1.1 alphabet invariant rules this case out for
///   well-formed bitstreams, so the reader treats it as a structural
///   error rather than a recoverable condition.
/// * For `n_stages == 0` the function returns
///   [`IndexBuffer::EMPTY`] without consuming any bits — this matches
///   the mean-only leaf path of spec/04 §4.5.4.
/// * Otherwise the function reads exactly `n_stages × 4` bits from
///   the underlying `BitReader`; each 4-bit chunk is interpreted as
///   a raw unsigned `vec_idx ∈ 0..=15`.
///
/// The reader propagates the underlying [`BitReader`]'s
/// [`Error::Truncated`] if the codec frame ends mid-index — a
/// per-spec mid-leaf cut-off that a malformed bitstream may exhibit.
///
/// ### Stage ordering
///
/// The first 4 bits go to stage-1, the next 4 bits to stage-2, …,
/// the last 4 bits to stage-`n_stages`. The wiki source pins this
/// arrangement in `wiki/Sorenson_Video_1.wiki` §"Decoding Intraframe
/// Plane Data": *"the first 4 bits specify the first codebook to
/// use, the next 4 bits specify the second codebook, all the way up
/// to a sixth codebooks for 6 possible stages"*.
///
/// ### Padding
///
/// Per spec/04 §4.2.1 there is **no inter-stage padding** — stage
/// `k+1`'s field begins at the bit immediately after stage `k`'s
/// field's last bit. This matches the [`BitReader`] convention of
/// MSB-first bit-tight consumption.
pub fn read_stage_indices(reader: &mut BitReader<'_>, n_stages: usize) -> Result<IndexBuffer> {
    if n_stages > MAX_STAGES_PER_LEAF {
        return Err(Error::BadBitWidth(n_stages as u32));
    }
    if n_stages == 0 {
        return Ok(IndexBuffer::EMPTY);
    }
    let mut buf = IndexBuffer::EMPTY;
    for slot in buf.indices.iter_mut().take(n_stages) {
        // Each field is a raw 4-bit unsigned integer per spec/04
        // §4.2.1. `BitReader::read_bits` returns a right-aligned
        // `u32`; the 4-bit value fits in a `u8` by definition.
        let vec_idx = reader.read_bits(BITS_PER_INDEX)?;
        // Defensive cast — bounded by `BITS_PER_INDEX = 4`.
        *slot = vec_idx as u8;
    }
    buf.len = n_stages as u8;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- bits_for_n_stages: closed-form arithmetic ---------------------

    #[test]
    fn bits_for_zero_stages_is_zero() {
        // Mean-only leaf path (spec/04 §4.5.4) — zero indices read.
        assert_eq!(bits_for_n_stages(0), Some(0));
    }

    #[test]
    fn bits_for_one_stage_is_four() {
        assert_eq!(bits_for_n_stages(1), Some(4));
    }

    #[test]
    fn bits_for_six_stages_is_twenty_four() {
        // Per spec/04 §4.2 table — max stage count consumes exactly
        // 6 × 4 = 24 bits, "(3 bytes worth, not necessarily byte-
        // aligned)" per §4.2.1.
        assert_eq!(bits_for_n_stages(MAX_STAGES_PER_LEAF), Some(24));
        assert_eq!(bits_for_n_stages(6), Some(24));
    }

    #[test]
    fn bits_for_seven_or_more_stages_is_none() {
        // The stage-count VLC alphabet excludes `N > 6`
        // (spec/04 §4.1.1) — the closed-form helper surfaces this
        // explicitly.
        assert_eq!(bits_for_n_stages(7), None);
        assert_eq!(bits_for_n_stages(usize::MAX), None);
    }

    #[test]
    fn bits_for_every_valid_n_is_four_times_n() {
        for n in 0..=MAX_STAGES_PER_LEAF {
            assert_eq!(bits_for_n_stages(n), Some(n * 4));
        }
    }

    // ---- IndexBuffer empty / get / accessors ---------------------------

    #[test]
    fn empty_buffer_has_zero_len() {
        let buf = IndexBuffer::EMPTY;
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
        assert!(buf.indices().is_empty());
    }

    #[test]
    fn empty_buffer_get_returns_none_for_any_stage() {
        let buf = IndexBuffer::EMPTY;
        assert_eq!(buf.get(0), None); // 0 is not a valid 1-based stage
        assert_eq!(buf.get(1), None);
        assert_eq!(buf.get(MAX_STAGES_PER_LEAF), None);
    }

    // ---- read_stage_indices: degenerate cases --------------------------

    #[test]
    fn read_zero_stages_consumes_no_bits() {
        let bytes = [0xAA, 0xBB];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, 0).unwrap();
        assert!(buf.is_empty());
        assert_eq!(reader.bits_consumed(), 0);
    }

    #[test]
    fn read_seven_stages_rejects_with_bad_bit_width() {
        // `N > MAX_STAGES_PER_LEAF` is rejected with a structural
        // error since the stage-count alphabet rules it out.
        let bytes = [0xFF; 4];
        let mut reader = BitReader::new(&bytes);
        let err = read_stage_indices(&mut reader, 7).unwrap_err();
        assert!(matches!(err, Error::BadBitWidth(7)));
        // Reader must not have advanced.
        assert_eq!(reader.bits_consumed(), 0);
    }

    #[test]
    fn read_eight_stages_rejects_with_bad_bit_width_carrying_count() {
        let bytes = [0xFF; 4];
        let mut reader = BitReader::new(&bytes);
        let err = read_stage_indices(&mut reader, 8).unwrap_err();
        assert!(matches!(err, Error::BadBitWidth(8)));
    }

    // ---- read_stage_indices: single-stage cases -----------------------

    #[test]
    fn one_stage_reads_high_nibble_of_first_byte() {
        // `0x5A = 0b0101_1010`. The high nibble = 5, the low nibble
        // = 10. With `N=1` only the high nibble is consumed.
        let bytes = [0x5A];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, 1).unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.indices(), &[5]);
        assert_eq!(buf.get(1), Some(5));
        assert_eq!(buf.get(2), None);
        assert_eq!(reader.bits_consumed(), 4);
    }

    #[test]
    fn one_stage_reads_full_15_high_nibble() {
        // Verifies the upper edge of the `vec_idx ∈ 0..=15` range.
        let bytes = [0xF0];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, 1).unwrap();
        assert_eq!(buf.indices(), &[MAX_VEC_IDX]);
    }

    #[test]
    fn one_stage_reads_zero_high_nibble() {
        // Verifies the lower edge — bitstream may legitimately
        // encode vec_idx = 0 (codebook entry 0 is just as valid as
        // entry 15).
        let bytes = [0x0F];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, 1).unwrap();
        assert_eq!(buf.indices(), &[0]);
    }

    // ---- read_stage_indices: multi-stage cases ------------------------

    #[test]
    fn two_stages_read_both_nibbles_of_first_byte() {
        // `0x5A = 0b0101_1010` → stage-1 = 5 (high nibble), stage-2
        // = 10 (low nibble). Per spec/04 §4.2 the stage-1 field
        // ALWAYS precedes stage-2; the bit-tight reader places
        // stage-1 in the high bits.
        let bytes = [0x5A];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, 2).unwrap();
        assert_eq!(buf.indices(), &[5, 10]);
        assert_eq!(buf.get(1), Some(5));
        assert_eq!(buf.get(2), Some(10));
        assert_eq!(reader.bits_consumed(), 8);
    }

    #[test]
    fn three_stages_cross_byte_boundary() {
        // Bytes `0x5A 0xB0` → high nibble 5, low nibble A, high
        // nibble B. The third stage's 4-bit field straddles the
        // byte boundary in the sense that the parser must continue
        // consuming bits past byte 0 without re-aligning.
        let bytes = [0x5A, 0xB0];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, 3).unwrap();
        assert_eq!(buf.indices(), &[5, 10, 11]);
        assert_eq!(reader.bits_consumed(), 12);
    }

    #[test]
    fn six_stages_consume_exactly_24_bits() {
        // `0x12 0x34 0x56` → nibbles 1, 2, 3, 4, 5, 6.
        let bytes = [0x12, 0x34, 0x56];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, MAX_STAGES_PER_LEAF).unwrap();
        assert_eq!(buf.indices(), &[1, 2, 3, 4, 5, 6]);
        assert_eq!(reader.bits_consumed(), 24);
        assert_eq!(reader.bits_consumed(), bits_for_n_stages(6).unwrap());
    }

    #[test]
    fn six_stages_all_maximum_vec_idx() {
        // `0xFF 0xFF 0xFF` → every nibble = 15.
        let bytes = [0xFF, 0xFF, 0xFF];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, MAX_STAGES_PER_LEAF).unwrap();
        assert_eq!(buf.indices(), &[15, 15, 15, 15, 15, 15]);
    }

    #[test]
    fn six_stages_all_zero_vec_idx() {
        let bytes = [0x00, 0x00, 0x00];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, MAX_STAGES_PER_LEAF).unwrap();
        assert_eq!(buf.indices(), &[0, 0, 0, 0, 0, 0]);
    }

    // ---- read_stage_indices: bit-tight, no inter-stage padding --------

    #[test]
    fn no_inter_stage_padding_observable_via_consumed_count() {
        // Per spec/04 §4.2.1 *"No padding: stage k+1's 4-bit field
        // begins at the bit immediately after stage k's field's last
        // bit"*. With `N=4` (16 bits) the reader cursor must land at
        // exactly bit 16.
        let bytes = [0x12, 0x34];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, 4).unwrap();
        assert_eq!(buf.indices(), &[1, 2, 3, 4]);
        assert_eq!(reader.bits_consumed(), 16);
    }

    // ---- read_stage_indices: continuation reads ----------------------

    #[test]
    fn subsequent_bit_read_after_n_stages_starts_at_correct_offset() {
        // Bytes `0xAB 0xCD` → stage-1 = A (10), stage-2 = B (11).
        // After reading 2 stages, the bit reader sits at bit 8. The
        // very next 4-bit read MUST return C (12) — proving that the
        // stage reader leaves no inter-stage padding behind it.
        let bytes = [0xAB, 0xCD];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, 2).unwrap();
        assert_eq!(buf.indices(), &[10, 11]);
        assert_eq!(reader.bits_consumed(), 8);
        // Read the next nibble manually and confirm it's `C`.
        let next_nibble = reader.read_bits(4).unwrap();
        assert_eq!(next_nibble, 0xC);
        assert_eq!(reader.bits_consumed(), 12);
    }

    // ---- read_stage_indices: truncation handling ---------------------

    #[test]
    fn truncation_mid_stream_returns_truncated_error() {
        // 6 stages require 3 bytes; supplying only 2 bytes triggers
        // a truncation on the fifth stage's 4-bit read (which falls
        // in byte 2 = beyond the slice).
        let bytes = [0x12, 0x34];
        let mut reader = BitReader::new(&bytes);
        let err = read_stage_indices(&mut reader, MAX_STAGES_PER_LEAF).unwrap_err();
        assert!(matches!(err, Error::Truncated));
    }

    #[test]
    fn truncation_at_first_stage_returns_truncated_error() {
        // Empty backing slice → first 4-bit read fails immediately.
        let bytes: [u8; 0] = [];
        let mut reader = BitReader::new(&bytes);
        let err = read_stage_indices(&mut reader, 1).unwrap_err();
        assert!(matches!(err, Error::Truncated));
    }

    #[test]
    fn truncation_mid_third_stage_returns_truncated_error() {
        // 3 stages need 12 bits; provide 8 bits (one full byte).
        let bytes = [0x12];
        let mut reader = BitReader::new(&bytes);
        let err = read_stage_indices(&mut reader, 3).unwrap_err();
        assert!(matches!(err, Error::Truncated));
    }

    // ---- IndexBuffer::get: one-based indexing -------------------------

    #[test]
    fn buffer_get_zero_returns_none_per_one_based_convention() {
        let bytes = [0x12];
        let mut reader = BitReader::new(&bytes);
        let buf = read_stage_indices(&mut reader, 2).unwrap();
        assert_eq!(buf.get(0), None);
        assert_eq!(buf.get(1), Some(1));
        assert_eq!(buf.get(2), Some(2));
        assert_eq!(buf.get(3), None);
    }

    // ---- Constants invariants -----------------------------------------

    #[test]
    fn max_vec_idx_is_fifteen() {
        // Sixteen vectors per stage per spec/14 §14.4, indexed
        // 0..=15.
        assert_eq!(MAX_VEC_IDX, 15);
        assert_eq!(MAX_VEC_IDX as usize, (1 << BITS_PER_INDEX) - 1);
    }

    #[test]
    fn max_stages_per_leaf_matches_codebook_module_constant() {
        // Cross-module consistency: the codebook payload has six
        // stages per level (spec/14 §14.4), so the index reader
        // must accept exactly that many stages.
        assert_eq!(
            MAX_STAGES_PER_LEAF,
            crate::svq1_codebook::SVQ1_STAGES_PER_LEVEL
        );
    }

    #[test]
    fn bits_per_index_is_four() {
        // Sanity check on the spec's "4 bits per index field"
        // constant — value-typed to surface this in changelogs and
        // grep-friendly searches.
        assert_eq!(BITS_PER_INDEX, 4);
    }
}
