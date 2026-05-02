//! Frame-level bitstream parsing for SVQ1.
//!
//! Implements §3 + §4 of `docs/video/svq1/svq1-trace-reverse-engineering.md`:
//!
//! * 22-bit `frame_code` packet prefix, restricted to
//!   `{0x20, 0x40, 0x50, 0x60}`.
//! * `temporal_reference: u(8)` + `frame_type: u(2)`.
//! * I-frame branch: optional 16-bit packet checksum (when
//!   `frame_code in {0x50, 0x60}`), optional scrambled embedded ASCII
//!   string (when `(frame_code ^ 0x10) >= 0x50`), five reserved bits
//!   (`u(2) u(2) u(1)`), `frame_size_code: u(3)`. Code `7` adds two
//!   `u(12)` fields for explicit width/height; codes `0..=6` look up a
//!   conceptual preset table (see [`PRESET_FRAME_SIZES`]).
//! * Common tail: `checksum_block_flag: u(1)` (then 4 bits if set);
//!   `extra_data_block_flag: u(1)` (then 8 reserved bits + a stop-1+u(8)
//!   chain if set).
//!
//! The body bits begin immediately after the header — there is **no
//! byte-alignment** before the per-plane raster.

use oxideav_core::bits::BitReader;
use oxideav_core::{Error, Result};

/// Decoded SVQ1 frame header. The subset of fields needed by the body
/// decoder; reserved-but-consumed bits are not exposed.
#[derive(Clone, Copy, Debug)]
pub struct FrameHeader {
    /// 22-bit packet prefix. One of `0x20`, `0x40`, `0x50`, `0x60`.
    pub frame_code: u32,
    /// Per-frame timestamp / playback order, 8-bit unsigned, wraps at 256.
    pub temporal_reference: u8,
    pub frame_type: FrameType,
    /// Frame width in pixels. For P-frames the caller must propagate
    /// the size from the reference; we set this to `0` so callers can
    /// detect a missing reference.
    pub width: u16,
    pub height: u16,
}

impl FrameHeader {
    /// True if this is an I-frame (independent / GOP boundary).
    pub fn is_intra(&self) -> bool {
        matches!(self.frame_type, FrameType::Intra)
    }

    /// Whether this frame should be retained as the predictor for the
    /// next frame. Mirrors the `frame_type != 2` rule.
    pub fn is_reference(&self) -> bool {
        !matches!(self.frame_type, FrameType::PNonReference)
    }
}

/// 2-bit `frame_type` field — see §4.2 of the trace doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameType {
    /// `0` — I-frame. Independently decodable.
    Intra,
    /// `1` — P-frame, reference (kept for the next frame's predictor).
    Predicted,
    /// `2` — P-frame, **non-reference**. Decoded and emitted, but
    /// `prev` is not updated afterwards.
    PNonReference,
    // `3` is "reserved" / rejected by the parser, never represented here.
}

/// Parse a complete SVQ1 frame header from the start of `data`. The
/// returned reader is positioned at the first body bit.
///
/// `prev_dims` is the dimensions of the decoder's reference frame (the
/// last decoded I- or reference-P-frame). P-frames inherit those
/// dimensions because the bitstream does not re-emit them.
pub fn parse_header<'a>(
    data: &'a [u8],
    prev_dims: Option<(u16, u16)>,
) -> Result<(FrameHeader, BitReader<'a>)> {
    let mut br = BitReader::new(data);

    // ── §3 packet prefix ──
    let frame_code = br.read_u32(22)?;
    if !is_legal_frame_code(frame_code) {
        return Err(Error::invalid(
            "svq1: frame_code outside legal set {0x20, 0x40, 0x50, 0x60}",
        ));
    }

    // §3.1 obfuscation: when frame_code != 0x20, bytes [4..36) are
    // de-obfuscated in place. Our test corpus is all-0x20 so we surface
    // a clear unsupported error rather than half-implementing the
    // pre-pass — see CHANGELOG for the deferral.
    if frame_code != 0x20 {
        return Err(Error::unsupported(
            "svq1: obfuscated header (frame_code != 0x20) not yet implemented",
        ));
    }

    // ── §4 frame header ──
    let temporal_reference = br.read_u32(8)? as u8;
    let frame_type_raw = br.read_u32(2)?;
    let frame_type = match frame_type_raw {
        0 => FrameType::Intra,
        1 => FrameType::Predicted,
        2 => FrameType::PNonReference,
        _ => return Err(Error::invalid("svq1: reserved frame_type=3")),
    };

    let (width, height) = if frame_type == FrameType::Intra {
        // §4.3 optional packet checksum
        if frame_code == 0x50 || frame_code == 0x60 {
            // Skip 16-bit CRC-16; we don't validate it.
            br.skip(16)?;
        }
        // §4.4 optional embedded scrambled string
        if (frame_code ^ 0x10) >= 0x50 {
            let len = br.read_u32(8)? as usize;
            // Each byte is u(8), XORed into a rolling seed via a
            // permutation LUT. We only need to consume the bits.
            br.skip((len * 8) as u32)?;
        }

        // Five reserved bits — encoder writes 0b10, 0, 0.
        let _r2a = br.read_u32(2)?;
        let _r2b = br.read_u32(2)?;
        let _r1 = br.read_u32(1)?;

        let size_code = br.read_u32(3)?;
        if size_code == 7 {
            let w = br.read_u32(12)? as u16;
            let h = br.read_u32(12)? as u16;
            if w == 0 || h == 0 {
                return Err(Error::invalid("svq1: explicit width/height zero"));
            }
            (w, h)
        } else {
            preset_size(size_code)?
        }
    } else {
        // P-frames: dimensions inherited from reference.
        prev_dims.ok_or_else(|| {
            Error::invalid("svq1: P-frame received before any I-frame established dimensions")
        })?
    };

    // ── §4.5 + §4 tail: optional blocks ──
    let checksum_block_flag = br.read_u32(1)? != 0;
    if checksum_block_flag {
        // u(1) use_packet_chk + u(1) use_comp_chk + u(2) reserved
        br.skip(4)?;
    }
    let extra_data_block_flag = br.read_u32(1)? != 0;
    if extra_data_block_flag {
        // 8 reserved bits then a stop-1 + u(8) chain.
        br.skip(8)?;
        // Stop-1 chain: each byte is preceded by a 0; a leading 1 ends.
        loop {
            let stop = br.read_u32(1)?;
            if stop == 1 {
                break;
            }
            br.skip(8)?;
        }
    }

    let header = FrameHeader {
        frame_code,
        temporal_reference,
        frame_type,
        width,
        height,
    };
    Ok((header, br))
}

/// `frame_code` legality test — §3 of the trace doc.
///
/// "none of the bits outside the mask `0x70` may be set, and at least
/// one of the bits in the mask `0x60` must be set" — yields the
/// four-element set `{0x20, 0x40, 0x50, 0x60}`.
fn is_legal_frame_code(fc: u32) -> bool {
    (fc & !0x70) == 0 && (fc & 0x60) != 0
}

/// The seven preset frame-size codes — §4.1 of the trace doc.
///
/// We seed entries `0..=6` from the corpus the trace observed (codes
/// `0, 1, 2, 3, 5, 6`); code `4` never appeared in the trace corpus
/// and is rejected at parse time rather than guessed at. Code `7` is
/// "explicit" and is handled in [`parse_header`] directly, not via
/// this table.
pub const PRESET_FRAME_SIZES: [Option<(u16, u16)>; 7] = [
    Some((160, 120)), // 0
    Some((128, 96)),  // 1
    Some((176, 144)), // 2
    Some((352, 288)), // 3
    None,             // 4 — not exercised by the trace corpus
    Some((240, 180)), // 5
    Some((320, 240)), // 6
];

fn preset_size(code: u32) -> Result<(u16, u16)> {
    PRESET_FRAME_SIZES
        .get(code as usize)
        .copied()
        .flatten()
        .ok_or_else(|| {
            Error::unsupported(format!(
                "svq1: preset frame_size_code {code} not in our trace-observed set"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny helper for synthesising bitstreams in tests. Each call
    /// appends `(value, nbits)` MSB-first.
    pub(crate) fn build_bytes(chunks: &[(u64, u32)]) -> (Vec<u8>, u32) {
        let mut acc: u128 = 0;
        let mut bits: u32 = 0;
        for &(v, n) in chunks {
            acc = (acc << n) | (v as u128);
            bits += n;
        }
        let pad = (8 - bits % 8) % 8;
        acc <<= pad;
        let total_bytes = ((bits + pad) / 8) as usize;
        let mut out = vec![0u8; total_bytes];
        for i in 0..total_bytes {
            let shift = (total_bytes - 1 - i) * 8;
            out[i] = ((acc >> shift) & 0xff) as u8;
        }
        (out, bits)
    }

    /// Build a minimal valid I-frame header for `frame_code = 0x20`,
    /// `temporal_reference = 0`, `frame_type = Intra`,
    /// `frame_size_code = 0` (160x120), no optional blocks.
    ///
    /// Bit layout:
    ///   22 bits frame_code = 0x20            → 0000_0000_0000_0000_001000
    ///    8 bits temporal_reference = 0       → 0000_0000
    ///    2 bits frame_type = 0 (I)           → 00
    ///    2 bits reserved = 0b10              → 10
    ///    2 bits reserved = 0                 → 00
    ///    1 bit  reserved = 0                 → 0
    ///    3 bits frame_size_code = 0          → 000
    ///    1 bit  checksum_block_flag = 0      → 0
    ///    1 bit  extra_data_block_flag = 0    → 0
    /// Total = 42 bits = 6 bytes (with 6 trailing pad bits).
    fn build_minimal_iframe() -> Vec<u8> {
        let chunks = [
            (0x20, 22),
            (0, 8),
            (0, 2),
            (0b10, 2),
            (0, 2),
            (0, 1),
            (0, 3),
            (0, 1),
            (0, 1),
        ];
        let (mut out, _bits) = build_bytes(&chunks);
        // Append a couple of body bytes so the reader has something to
        // walk past the header.
        out.extend_from_slice(&[0u8; 4]);
        out
    }

    #[test]
    fn parses_minimal_iframe() {
        let bytes = build_minimal_iframe();
        let (hdr, br) = parse_header(&bytes, None).unwrap();
        assert_eq!(hdr.frame_code, 0x20);
        assert_eq!(hdr.temporal_reference, 0);
        assert_eq!(hdr.frame_type, FrameType::Intra);
        assert_eq!((hdr.width, hdr.height), (160, 120));
        // Header is exactly 42 bits.
        assert_eq!(br.bit_position(), 42);
    }

    #[test]
    fn rejects_obfuscated_frame_code() {
        // Build a 22-bit prefix encoding frame_code = 0x40 (legal but
        // we don't yet implement the obfuscation pre-pass).
        let chunks = [(0x40u64, 22)];
        let (bytes, _) = build_bytes(&chunks);
        let err = match parse_header(&bytes, None) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("obfuscated"), "msg = {msg}");
    }

    #[test]
    fn rejects_invalid_frame_code() {
        // frame_code = 0x10 has no bits set inside 0x60 → fails the
        // "at least one bit in 0x60 must be set" test.
        let chunks = [(0x10u64, 22)];
        let (bytes, _) = build_bytes(&chunks);
        let err = match parse_header(&bytes, None) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("frame_code") && msg.contains("legal"),
            "msg = {msg}"
        );
    }

    #[test]
    fn explicit_size_path() {
        let chunks = [
            (0x20, 22),
            (0, 8),
            (0, 2),
            (0b10, 2),
            (0, 2),
            (0, 1),
            (7, 3),
            (200, 12),
            (150, 12),
            (0, 1),
            (0, 1),
        ];
        let (mut out, _bits) = build_bytes(&chunks);
        out.extend_from_slice(&[0u8; 4]);

        let (hdr, br) = parse_header(&out, None).unwrap();
        assert_eq!((hdr.width, hdr.height), (200, 150));
        // Explicit-size header is 22 + 8 + 2 + 5 + 3 + 24 + 1 + 1 = 66 bits.
        assert_eq!(br.bit_position(), 66);
    }

    #[test]
    fn pframe_inherits_dims_from_prev() {
        let chunks = [
            (0x20, 22),
            (0, 8),
            (1, 2), // frame_type = 1 (P)
            (0, 1), // checksum_block_flag
            (0, 1), // extra_data_block_flag
        ];
        let (mut out, _bits) = build_bytes(&chunks);
        out.extend_from_slice(&[0u8; 4]);

        let (hdr, _) = parse_header(&out, Some((352, 288))).unwrap();
        assert_eq!(hdr.frame_type, FrameType::Predicted);
        assert_eq!((hdr.width, hdr.height), (352, 288));
    }

    #[test]
    fn pframe_without_prev_rejected() {
        let chunks = [(0x20, 22), (0, 8), (1, 2), (0, 1), (0, 1)];
        let (mut out, _bits) = build_bytes(&chunks);
        out.extend_from_slice(&[0u8; 4]);
        assert!(parse_header(&out, None).is_err());
    }
}
