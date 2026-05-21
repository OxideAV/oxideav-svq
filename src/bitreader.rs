//! MSB-first bit reader used by the SVQ1 frame-header parser.
//!
//! The SVQ1 frame header described in
//! `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
//! Header" is a bit-packed structure that begins with a 22-bit frame
//! code at the very first bit of the chunk; the subsequent fields are
//! all sub-byte and the parser therefore needs a generic
//! variable-width bit reader.
//!
//! The wiki spec does not explicitly call out bit ordering, but
//! describes the 22-bit frame code by its hexadecimal value with the
//! upper hex nibbles tied to the MSBs of the chunk's first byte —
//! e.g. "frame code is `0x50`" treats `0x50` as the high 22 bits, so
//! bit 7 of byte 0 is the first bit consumed. This matches the MSB-
//! first convention used by every other SVQ-family bitstream
//! representation in the document and is the natural reading of a
//! "frame code" expressed as a hexadecimal constant in a big-endian
//! wire layout.
//!
//! Round 1 needs three primitives:
//!
//! * [`BitReader::peek_bit`] — look at the next bit without consuming
//!   it.  Used by the "`while (next bit is 1) read a byte`" tail of
//!   the wiki spec's optional-trailer loop.
//! * [`BitReader::read_bit`] — consume a single bit.
//! * [`BitReader::read_bits`] — consume `n` bits (1..=32) and return
//!   them right-aligned in a `u32`.
//!
//! No write side, no signed reads, no Golomb / VLC machinery —
//! everything beyond the structural header is out of round-1 scope.

use crate::error::{Error, Result};

/// MSB-first bit reader over a borrowed byte slice.
///
/// The first call to [`BitReader::read_bit`] returns bit 7 (mask
/// `0x80`) of `bytes[0]`. Subsequent reads progress through bit 6,
/// bit 5, …, bit 0 of `bytes[0]`, then bit 7 of `bytes[1]`, and so on.
#[derive(Debug)]
pub struct BitReader<'a> {
    bytes: &'a [u8],
    /// Bit position relative to the start of `bytes` (i.e. bit 0 is
    /// bit 7 of `bytes[0]`).
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    /// Build a fresh reader positioned at bit 0 of `bytes`.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, bit_pos: 0 }
    }

    /// Total bits available in the backing slice.
    pub fn total_bits(&self) -> usize {
        self.bytes.len() * 8
    }

    /// Bits already consumed by `read_*` calls.
    pub fn bits_consumed(&self) -> usize {
        self.bit_pos
    }

    /// Bits remaining in the backing slice.
    pub fn bits_remaining(&self) -> usize {
        self.total_bits().saturating_sub(self.bit_pos)
    }

    /// Look at the next bit (0 or 1) without consuming it. Returns
    /// [`Error::Truncated`] if no further bits are available.
    pub fn peek_bit(&self) -> Result<u8> {
        if self.bit_pos >= self.total_bits() {
            return Err(Error::Truncated);
        }
        let byte = self.bytes[self.bit_pos / 8];
        let shift = 7 - (self.bit_pos % 8);
        Ok((byte >> shift) & 1)
    }

    /// Consume and return the next bit (0 or 1). Returns
    /// [`Error::Truncated`] if no further bits are available.
    pub fn read_bit(&mut self) -> Result<u8> {
        let b = self.peek_bit()?;
        self.bit_pos += 1;
        Ok(b)
    }

    /// Consume `n` bits and return them right-aligned in a `u32`.
    /// `n` must be in `1..=32`; the wiki spec's longest single
    /// integer field is the 22-bit frame code, but the loop tail of
    /// the optional trailer reads one byte at a time and the parser
    /// uses the 12-bit width / height pair, so accepting up to 32
    /// bits covers every present and reasonably-foreseeable future
    /// fixed-width read.
    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        if n == 0 || n > 32 {
            return Err(Error::BadBitWidth(n));
        }
        if self.bits_remaining() < n as usize {
            return Err(Error::Truncated);
        }
        let mut value: u32 = 0;
        for _ in 0..n {
            value = (value << 1) | (self.read_bit()? as u32);
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_bit_is_msb_first() {
        // 0b1010_0110 = 0xA6
        let mut br = BitReader::new(&[0xA6]);
        let bits: Vec<u8> = (0..8).map(|_| br.read_bit().unwrap()).collect();
        assert_eq!(bits, vec![1, 0, 1, 0, 0, 1, 1, 0]);
        assert!(matches!(br.read_bit(), Err(Error::Truncated)));
    }

    #[test]
    fn peek_does_not_advance() {
        let mut br = BitReader::new(&[0x80]);
        assert_eq!(br.peek_bit().unwrap(), 1);
        assert_eq!(br.peek_bit().unwrap(), 1);
        assert_eq!(br.bits_consumed(), 0);
        assert_eq!(br.read_bit().unwrap(), 1);
        assert_eq!(br.peek_bit().unwrap(), 0);
    }

    #[test]
    fn read_bits_packs_msb_first() {
        // Byte sequence 0b1100_1010 0b1111_0000 → first 12 bits =
        // 0b1100_1010_1111 = 0xCAF.
        let mut br = BitReader::new(&[0xCA, 0xF0]);
        assert_eq!(br.read_bits(12).unwrap(), 0xCAF);
        assert_eq!(br.bits_consumed(), 12);
    }

    #[test]
    fn read_bits_across_byte_boundary() {
        // 22-bit frame code 0x50 in the high 22 bits + 10 low zero
        // bits ⇒ 0x50 << 10 = 0x14000, which packs as bytes
        // [0x14, 0x00, 0x00] (since 0x50 occupies bits 31..10 of a
        // 32-bit big-endian shift). To make that easy to reason
        // about we instead encode 0x50 in the *high* 22 bits of the
        // backing slice: 0x14, 0x00, 0x00 → read 22 bits → 0x050000.
        let mut br = BitReader::new(&[0x14, 0x00, 0x00]);
        let value = br.read_bits(22).unwrap();
        // bits 23..2 of the slice = 0x14_00_00 >> 2 = 0x050000.
        assert_eq!(value, 0x05_0000);
    }

    #[test]
    fn read_bits_truncated_returns_error() {
        let mut br = BitReader::new(&[0xFF]);
        assert!(matches!(br.read_bits(9), Err(Error::Truncated)));
    }

    #[test]
    fn read_bits_zero_width_rejected() {
        let mut br = BitReader::new(&[0xFF]);
        assert!(matches!(br.read_bits(0), Err(Error::BadBitWidth(0))));
    }

    #[test]
    fn read_bits_over_32_rejected() {
        let mut br = BitReader::new(&[0; 8]);
        assert!(matches!(br.read_bits(33), Err(Error::BadBitWidth(33))));
    }
}
