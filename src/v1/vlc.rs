//! Variable-Length-Code (Huffman) decoder for the SVQ1 VLC tables.
//!
//! SVQ1 stores its tables as `(code, length, value)` triples in
//! [`crate::v1::tables`], transcribed verbatim from §14 of
//! `docs/video/svq1/svq1-trace-reverse-engineering.md`. Codes are
//! read **MSB-first** from the bitstream.
//!
//! The tables are small (4..512 entries) but the longest code can be
//! 22 bits (inter mean). A sequential per-length scan is well within
//! decoder budget and matches the pattern used in
//! `oxideav-jpeg2000` and `oxideav-jpegxl` — no canonical-Huffman
//! preprocessing required.

use oxideav_core::bits::BitReader;
use oxideav_core::{Error, Result};

use super::tables::VlcEntry;

/// A built VLC decoder for one SVQ1 table.
///
/// Internally the entries are sorted by length ascending so the
/// decoder can read bits one at a time, build up an accumulator, and
/// search for matches at each bit length until a hit is found.
pub struct Vlc {
    /// Sorted by length ascending; `(length, code, value)`.
    entries: Vec<(u8, u32, i32)>,
    /// Maximum code length present (bits).
    max_len: u8,
    /// Minimum code length present (bits) — the inner loop primes the
    /// accumulator with this many bits before any match attempt.
    min_len: u8,
}

impl Vlc {
    /// Build a VLC decoder from a table of `(code, length, value)` triples.
    pub fn new(table: &[VlcEntry]) -> Self {
        let mut entries: Vec<(u8, u32, i32)> = table
            .iter()
            .map(|&(code, length, value)| (length, code, value))
            .collect();
        entries.sort_by_key(|&(len, code, _)| (len, code));
        let max_len = entries.iter().map(|e| e.0).max().unwrap_or(0);
        let min_len = entries.first().map(|e| e.0).unwrap_or(0);
        Self {
            entries,
            max_len,
            min_len,
        }
    }

    /// Decode the next symbol from `br`. Reads MSB-first; on error
    /// (no match within `max_len` bits, or bit-reader underflow)
    /// returns the underlying error.
    pub fn decode(&self, br: &mut BitReader<'_>) -> Result<i32> {
        let mut acc: u32 = 0;
        let mut have: u8 = 0;
        // Prime with the minimum length bits — no shorter code can
        // possibly match.
        for _ in 0..self.min_len {
            acc = (acc << 1) | br.read_u32(1)?;
            have += 1;
        }
        loop {
            // Search the slice at this length.
            for &(len, code, sym) in &self.entries {
                if len == have && code == acc {
                    return Ok(sym);
                }
                if len > have {
                    break;
                }
            }
            if have >= self.max_len {
                return Err(Error::invalid("svq1 vlc: no match within max code length"));
            }
            acc = (acc << 1) | br.read_u32(1)?;
            have += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::tables;
    use oxideav_core::bits::{BitReader, BitWriter};

    fn write_code(bw: &mut BitWriter, code: u32, length: u8) {
        bw.write_u32(code, length as u32);
    }

    /// Round-trip every entry in a VLC table: encode the bare code and
    /// decode it back. Pads with zero bits past the end of the codeword
    /// so the reader doesn't underflow when searching past the match.
    fn roundtrip_table(table: &[VlcEntry]) {
        let vlc = Vlc::new(table);
        for &(code, length, expected) in table {
            let mut bw = BitWriter::new();
            write_code(&mut bw, code, length);
            // Pad past max so reader never underflows.
            bw.write_u32(0, 24);
            let bytes = bw.into_bytes();
            let mut br = BitReader::new(&bytes);
            let got = vlc.decode(&mut br).expect("decode");
            assert_eq!(
                got, expected,
                "code={code:b} len={length} expected {expected} got {got}"
            );
            // The reader should be positioned exactly at the end of
            // the codeword.
            assert_eq!(br.bit_position(), length as u64);
        }
    }

    #[test]
    fn block_type_roundtrip() {
        roundtrip_table(tables::BLOCK_TYPE_VLC);
    }

    #[test]
    fn intra_multistage_all_levels_roundtrip() {
        for tbl in tables::INTRA_MULTISTAGE_VLC.iter() {
            roundtrip_table(tbl);
        }
    }

    #[test]
    fn inter_multistage_all_levels_roundtrip() {
        for tbl in tables::INTER_MULTISTAGE_VLC.iter() {
            roundtrip_table(tbl);
        }
    }

    #[test]
    fn intra_mean_full_roundtrip() {
        roundtrip_table(tables::INTRA_MEAN_VLC);
    }

    #[test]
    fn inter_mean_full_roundtrip() {
        roundtrip_table(tables::INTER_MEAN_VLC);
    }

    #[test]
    fn mv_component_roundtrip() {
        roundtrip_table(tables::MV_COMPONENT_VLC);
    }

    #[test]
    fn block_type_known_codes() {
        let vlc = Vlc::new(tables::BLOCK_TYPE_VLC);
        // 1 → 0 (SKIP)
        let bytes = [0b1000_0000];
        let mut br = BitReader::new(&bytes);
        assert_eq!(vlc.decode(&mut br).unwrap(), 0);
        // 01 → 1 (INTER)
        let bytes = [0b0100_0000];
        let mut br = BitReader::new(&bytes);
        assert_eq!(vlc.decode(&mut br).unwrap(), 1);
        // 001 → 2 (INTER_4V)
        let bytes = [0b0010_0000];
        let mut br = BitReader::new(&bytes);
        assert_eq!(vlc.decode(&mut br).unwrap(), 2);
        // 000 → 3 (INTRA)
        let bytes = [0b0000_0000];
        let mut br = BitReader::new(&bytes);
        assert_eq!(vlc.decode(&mut br).unwrap(), 3);
    }
}
