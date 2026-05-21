//! SVQ1 frame-header parser.
//!
//! Implements the bit-packed frame header described in
//! `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
//! Header". The wiki page is a verbatim local mirror of the
//! multimedia.cx Sorenson_Video_1 wiki page (fetched 2026-05-06, CC-
//! BY-SA per multimedia.cx terms) and is the only authoritative
//! source consulted during the round-1 implementation.
//!
//! The header lays out as the following sequence of bit-packed fields
//! (positions are bit offsets relative to the start of the chunk; the
//! parser reads MSB-first via [`crate::bitreader::BitReader`]):
//!
//! | Offset       | Width  | Field                                                   |
//! | ------------ | ------ | ------------------------------------------------------- |
//! | 0            | 22     | `frame_code` — abort with [`Error::InvalidFrameCode`] when `(frame_code & 0x60) == 0` |
//! | 22           | 8      | `temporal_reference`                                    |
//! | 30           | 2      | `picture_type` — `0`=I, `1`=P, `2`=B, `3`=invalid       |
//! | 32 (I-frame) | 16     | optional `checksum` if `frame_code` is `0x50` or `0x60` |
//! | … (I-frame)  | varies | optional embedded ASCII string if `frame_code ^ 0x10 > 0x50` |
//! | … (I-frame)  | 2+2+1+3 | three "unknown" sub-fields plus `frame_size_code`      |
//! | … (I-frame)  | 24     | optional explicit `frame_width:12 + frame_height:12` when `frame_size_code == 7` |
//! | …            | 1      | `checksum_present`                                      |
//! | … (if set)   | 4      | `use_packet_checksum:1 + component_checksums:1 + reserved2:2` (reserved2 must be 0) |
//! | …            | 1      | `unknown_flag_1`                                        |
//! | … (if set)   | 1+4+1+2 | four "unknown" sub-fields                              |
//! | … (if set)   | 0..*   | byte-padded trailer — `while next bit is 1 { read 8 bits }` |
//!
//! Round 1 returns a typed [`Svq1FrameHeader`] populated with the
//! structurally-decoded fields and reports the bit position at which
//! the encoded plane data of the wiki spec's §"Decoding Intraframe
//! Plane Data" / §"Decoding Interframe Plane Data" begins. The plane
//! data is **not** decoded yet — the codebook tables that drive the
//! per-block VQ stages are tracked separately under the
//! docs-collaborator task and are out of round-1 scope.
//!
//! ## What the parser does **not** do
//!
//! * It does not verify the `checksum` field; the wiki spec
//!   explicitly notes "The specific details of the checksum coding
//!   are not all known".
//! * It does not interpret the embedded ASCII string contents
//!   (`string_xor_table[]` is not yet transcribed; the parser
//!   captures the raw obfuscated bytes plus the declared length and
//!   leaves de-obfuscation to a follow-up round once the XOR table
//!   is pinned in `docs/`).
//! * It does not decode the encoded plane data that follows the
//!   header — codebook + VLC tables for the multistage VQ decode
//!   are blocked on the existing docs-collaborator task tracking
//!   the codebook byte-list extraction.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};

/// Frame-size-code lookup table from `docs/video/svq1/wiki/
/// Sorenson_Video_1.wiki` §"Stream Format And Header". Code `7` is
/// the escape-to-explicit-dimensions sentinel and is handled
/// separately (the parser reads two 12-bit width/height fields when
/// the code lands at `7`).
///
/// | code | width | height |
/// | ---- | ----- | ------ |
/// | 0    | 160   | 120    |
/// | 1    | 128   | 96     |
/// | 2    | 176   | 144    |
/// | 3    | 352   | 288    |
/// | 4    | 704   | 576    |
/// | 5    | 240   | 180    |
/// | 6    | 320   | 240    |
pub const FRAME_SIZE_TABLE: [(u16, u16); 7] = [
    (160, 120),
    (128, 96),
    (176, 144),
    (352, 288),
    (704, 576),
    (240, 180),
    (320, 240),
];

/// SVQ1 picture-type field. Per the wiki spec the 2-bit field at bit
/// offset 30 of the chunk encodes one of these four values; value `3`
/// is reserved and aborts the parse with [`Error::InvalidPictureType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq1PictureType {
    /// Intraframe — coded standalone, no inter-frame data dependency.
    /// Carries the optional checksum / embedded-string / explicit-
    /// dimensions trailer described by the wiki spec.
    Intra,
    /// Predicted frame — depends on the previous I- or P-frame's
    /// reconstructed plane data.
    Predicted,
    /// Droppable predicted ("B") frame. The wiki spec emphasises that
    /// SVQ1 B-frames are unidirectional (predicted from the previous
    /// I- or P-frame only), unlike MPEG B-frames.
    Droppable,
}

impl Svq1PictureType {
    /// Decode the 2-bit picture-type field per the wiki spec.
    pub fn from_bits(bits: u8) -> Result<Self> {
        match bits {
            0 => Ok(Self::Intra),
            1 => Ok(Self::Predicted),
            2 => Ok(Self::Droppable),
            3 => Err(Error::InvalidPictureType),
            _ => Err(Error::BadBitWidth(bits as u32)),
        }
    }
}

/// Optional checksum-trailer field group. Present iff
/// `checksum_present` is set in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChecksumTrailer {
    /// Per-chunk packet checksum flag.
    pub use_packet_checksum: bool,
    /// Per-plane component checksum flag.
    pub component_checksums: bool,
}

/// Optional embedded ASCII string from the I-frame trailer. The
/// string body is XOR-obfuscated with a per-byte XOR derived from a
/// lookup table that is not yet transcribed in `docs/`; the parser
/// captures the declared length plus the raw obfuscated bytes for a
/// later round to de-obfuscate once the table lands.
///
/// The declared length is stored in `length` (the first byte of the
/// embedded run, before XOR is applied — per the wiki spec the
/// length byte is itself unobfuscated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedString {
    /// Declared length in bytes (the value of `string[0]`, the
    /// unobfuscated length byte). Maximum 255 since the length is a
    /// single byte.
    pub length: u8,
    /// The remaining `length - 1` obfuscated body bytes, captured raw.
    /// Empty when `length == 0` or `length == 1`.
    pub obfuscated_body: Vec<u8>,
}

/// Parsed SVQ1 frame header.
///
/// Field naming and semantics follow `docs/video/svq1/wiki/
/// Sorenson_Video_1.wiki` §"Stream Format And Header" exactly; the
/// "unknown" sub-fields of the I-frame trailer and the
/// `unknown_flag_1` trailer are captured raw so a future round can
/// pin their semantics without re-walking the bitstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svq1FrameHeader {
    /// Raw 22-bit frame-code field at the start of the chunk.
    pub frame_code: u32,
    /// 8-bit temporal-reference counter (frame display ordering hint;
    /// the wiki spec does not specify the wrap semantics).
    pub temporal_reference: u8,
    /// Decoded 2-bit picture-type field.
    pub picture_type: Svq1PictureType,
    /// I-frame-only optional 16-bit checksum present when the frame
    /// code is exactly `0x50` or `0x60`.
    pub checksum: Option<u16>,
    /// I-frame-only optional XOR-obfuscated embedded ASCII string.
    pub embedded_string: Option<EmbeddedString>,
    /// I-frame-only 7-bit run of "unknown" sub-fields preceding the
    /// frame-size code (2 + 2 + 1 bits). Captured as a single
    /// integer with the bits ordered MSB-first as read.
    pub i_frame_unknown_prefix: Option<u8>,
    /// Decoded frame width in pixels. For non-I frames the field is
    /// absent in the bitstream; this returns `None` and downstream
    /// code is expected to carry the previously-decoded I-frame's
    /// dimensions forward.
    pub width: Option<u16>,
    /// Decoded frame height in pixels. See [`Self::width`].
    pub height: Option<u16>,
    /// Raw 3-bit frame-size-code field on I-frames. `7` indicates the
    /// width/height pair was carried explicitly in the bitstream.
    pub frame_size_code: Option<u8>,
    /// Optional structural checksum-trailer field group.
    pub checksum_trailer: Option<ChecksumTrailer>,
    /// Bit-pattern of the `unknown_flag_1`-gated 1+4+1+2 trailer when
    /// the flag was set, captured as a single integer in MSB-first
    /// order (so bit 7 is the first of the four sub-fields). When
    /// `unknown_flag_1` was clear this is `None`.
    pub unknown_flag_1_payload: Option<u8>,
    /// Captured "extra byte" trailer from the `unknown_flag_1` arm:
    /// while the next bit is `1`, read one byte. The captured bytes
    /// do not include the terminating zero peek-bit.
    pub unknown_flag_1_extras: Vec<u8>,
    /// Bit position (relative to the start of the chunk) at which the
    /// header ends and the encoded plane data of §"Decoding
    /// Intraframe Plane Data" / §"Decoding Interframe Plane Data"
    /// begins.
    pub header_end_bit: usize,
}

impl Svq1FrameHeader {
    /// Convenience predicate — `true` iff the frame is an I-frame.
    pub fn is_intra(&self) -> bool {
        self.picture_type == Svq1PictureType::Intra
    }
}

/// Parse the SVQ1 frame header from the start of `bytes`.
///
/// Returns the populated [`Svq1FrameHeader`] on success; returns a
/// crate-local [`Error`] for any of the four documented structural
/// failure conditions (invalid frame code, reserved picture-type
/// value `3`, non-zero `unknown_field_1`, or premature end of input).
pub fn parse_frame_header(bytes: &[u8]) -> Result<Svq1FrameHeader> {
    let mut br = BitReader::new(bytes);

    // frame code (22 bits)
    let frame_code = br.read_bits(22)?;
    // (frame_code & 0x60) == 0  →  invalid
    if (frame_code & 0x60) == 0 {
        return Err(Error::InvalidFrameCode(frame_code));
    }

    // temporal reference (8 bits)
    let temporal_reference = br.read_bits(8)? as u8;

    // picture type (2 bits)
    let picture_type_bits = br.read_bits(2)? as u8;
    let picture_type = Svq1PictureType::from_bits(picture_type_bits)?;

    // I-frame-only trailer.
    let mut checksum = None;
    let mut embedded_string = None;
    let mut i_frame_unknown_prefix = None;
    let mut width = None;
    let mut height = None;
    let mut frame_size_code = None;

    if picture_type == Svq1PictureType::Intra {
        // Optional 16-bit checksum when frame_code == 0x50 || 0x60.
        if frame_code == 0x50 || frame_code == 0x60 {
            checksum = Some(br.read_bits(16)? as u16);
        }

        // Optional embedded string when (frame_code ^ 0x10) > 0x50.
        if (frame_code ^ 0x10) > 0x50 {
            // First byte = length (unobfuscated).
            let length = br.read_bits(8)? as u8;
            // The wiki spec's loop iterates 1..length-1, i.e. it
            // reads `length - 1` obfuscated body bytes (when
            // `length >= 1`; for `length == 0` no body bytes are
            // read).
            let body_len = length.saturating_sub(1) as usize;
            let mut obfuscated_body = Vec::with_capacity(body_len);
            for _ in 0..body_len {
                obfuscated_body.push(br.read_bits(8)? as u8);
            }
            embedded_string = Some(EmbeddedString {
                length,
                obfuscated_body,
            });
        }

        // 2 + 2 + 1 = 5 bits of "unknown" sub-fields.
        let unk_a = br.read_bits(2)? as u8;
        let unk_b = br.read_bits(2)? as u8;
        let unk_c = br.read_bits(1)? as u8;
        // Pack MSB-first: aa bb c0_00 → top 5 bits used, low 3 zero.
        i_frame_unknown_prefix = Some((unk_a << 6) | (unk_b << 4) | (unk_c << 3));

        // 3-bit frame size code.
        let fsc = br.read_bits(3)? as u8;
        frame_size_code = Some(fsc);
        match fsc {
            7 => {
                let w = br.read_bits(12)? as u16;
                let h = br.read_bits(12)? as u16;
                width = Some(w);
                height = Some(h);
            }
            other => {
                // The lookup table covers codes 0..=6; `fsc <= 7`
                // from a 3-bit read so `other` is guaranteed in
                // 0..=6 here.
                let (w, h) = FRAME_SIZE_TABLE[other as usize];
                width = Some(w);
                height = Some(h);
            }
        }
    }

    // 1-bit checksum_present flag.
    let checksum_present = br.read_bit()? == 1;
    let checksum_trailer = if checksum_present {
        let use_packet_checksum = br.read_bit()? == 1;
        let component_checksums = br.read_bit()? == 1;
        let reserved2 = br.read_bits(2)?;
        if reserved2 != 0 {
            return Err(Error::InvalidChecksumTrailer);
        }
        Some(ChecksumTrailer {
            use_packet_checksum,
            component_checksums,
        })
    } else {
        None
    };

    // 1-bit unknown_flag_1.
    let unknown_flag_1 = br.read_bit()? == 1;
    let mut unknown_flag_1_payload = None;
    let mut unknown_flag_1_extras = Vec::new();

    if unknown_flag_1 {
        let unk_d = br.read_bit()?;
        let unk_e = br.read_bits(4)? as u8;
        let unk_f = br.read_bit()?;
        let unk_g = br.read_bits(2)? as u8;
        // Pack MSB-first: d eeee f gg
        unknown_flag_1_payload = Some((unk_d << 7) | (unk_e << 3) | (unk_f << 2) | unk_g);

        // while (next bit is 1) read a byte.
        // Each "byte" pulls 8 bits from the stream after consuming
        // the 1-bit flag.
        loop {
            let flag = br.read_bit()?;
            if flag == 0 {
                break;
            }
            unknown_flag_1_extras.push(br.read_bits(8)? as u8);
        }
    }

    Ok(Svq1FrameHeader {
        frame_code,
        temporal_reference,
        picture_type,
        checksum,
        embedded_string,
        i_frame_unknown_prefix,
        width,
        height,
        frame_size_code,
        checksum_trailer,
        unknown_flag_1_payload,
        unknown_flag_1_extras,
        header_end_bit: br.bits_consumed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: pack a sequence of `(width, value)` items into a
    /// byte stream by writing them MSB-first. Trailing zero bits
    /// pad the last byte.
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

    /// Common-case I-frame header: `frame_code = 0x40`, picture-type
    /// I, frame-size-code 2 (176×144 CIF). `0x40` is the smallest
    /// frame code that simultaneously (a) clears the
    /// `frame_code & 0x60 == 0` invalid check (bit 6 set), (b) is
    /// neither `0x50` nor `0x60` so the optional 16-bit checksum is
    /// absent, and (c) satisfies `frame_code ^ 0x10 <= 0x50`
    /// (`0x40 ^ 0x10 = 0x50`, not strictly greater than `0x50`) so
    /// the optional embedded-string branch stays inactive — i.e.
    /// the "all optional sub-fields off" baseline case.
    #[test]
    fn iframe_size_code_2_minimal() {
        // Layout (bits): [22 fc=0x40] [8 tref=0] [2 pt=0]
        // I-frame trailer: [2 0][2 0][1 0][3 fsc=2]
        // [1 chksum_present=0] [1 unk_flag1=0]
        let bytes = pack(&[
            (22, 0x40),
            (8, 0),
            (2, 0),
            (2, 0),
            (2, 0),
            (1, 0),
            (3, 2),
            (1, 0),
            (1, 0),
        ]);
        let h = parse_frame_header(&bytes).expect("valid I-frame header");
        assert_eq!(h.frame_code, 0x40);
        assert_eq!(h.temporal_reference, 0);
        assert_eq!(h.picture_type, Svq1PictureType::Intra);
        assert_eq!(h.frame_size_code, Some(2));
        assert_eq!(h.width, Some(176));
        assert_eq!(h.height, Some(144));
        assert!(h.checksum.is_none());
        assert!(h.embedded_string.is_none());
        assert!(h.checksum_trailer.is_none());
        assert!(h.unknown_flag_1_payload.is_none());
        assert!(h.is_intra());
        // 22 + 8 + 2 + 2 + 2 + 1 + 3 + 1 + 1 = 42
        assert_eq!(h.header_end_bit, 42);
    }

    /// Explicit-dimensions escape: frame-size-code 7 forces a 12+12
    /// trailer pair. `frame_code = 0x40` per the rationale on
    /// [`iframe_size_code_2_minimal`].
    #[test]
    fn iframe_size_code_7_explicit_dimensions() {
        let bytes = pack(&[
            (22, 0x40),
            (8, 42),
            (2, 0),
            (2, 0),
            (2, 0),
            (1, 0),
            (3, 7),
            (12, 640),
            (12, 480),
            (1, 0),
            (1, 0),
        ]);
        let h = parse_frame_header(&bytes).expect("valid I-frame header");
        assert_eq!(h.frame_size_code, Some(7));
        assert_eq!(h.width, Some(640));
        assert_eq!(h.height, Some(480));
        assert_eq!(h.temporal_reference, 42);
    }

    /// 0x50 frame code triggers the 16-bit checksum branch.
    #[test]
    fn iframe_fc50_has_checksum() {
        // (0x50 ^ 0x10) = 0x40 ≤ 0x50 so no embedded string fires.
        let bytes = pack(&[
            (22, 0x50),
            (8, 0),
            (2, 0),
            (16, 0xBEEF), // checksum
            (2, 0),
            (2, 0),
            (1, 0),
            (3, 0), // 160×120
            (1, 0),
            (1, 0),
        ]);
        let h = parse_frame_header(&bytes).expect("valid I-frame header with checksum");
        assert_eq!(h.frame_code, 0x50);
        assert_eq!(h.checksum, Some(0xBEEF));
        assert_eq!(h.width, Some(160));
        assert_eq!(h.height, Some(120));
    }

    /// 0x6F frame code: (0x6F ^ 0x10) = 0x7F > 0x50 ⇒ embedded
    /// string fires. The body bytes are returned raw (XOR table not
    /// yet pinned).
    #[test]
    fn iframe_fc6f_has_embedded_string() {
        // 3-byte string: length=3, body 0xAA 0xBB.
        let bytes = pack(&[
            (22, 0x6F),
            (8, 0),
            (2, 0),
            (8, 3),    // string length
            (8, 0xAA), // obfuscated body[0]
            (8, 0xBB), // obfuscated body[1]
            (2, 0),
            (2, 0),
            (1, 0),
            (3, 2), // 176×144
            (1, 0),
            (1, 0),
        ]);
        let h = parse_frame_header(&bytes).expect("valid I-frame header with embedded string");
        let embedded = h.embedded_string.expect("embedded string present");
        assert_eq!(embedded.length, 3);
        assert_eq!(embedded.obfuscated_body, vec![0xAA, 0xBB]);
    }

    /// P-frame: the I-frame trailer is skipped entirely.
    #[test]
    fn pframe_skips_i_trailer() {
        // [22 fc=0x6F] [8 tref=5] [2 pt=1]
        // [1 chksum_present=0] [1 unk_flag1=0]
        let bytes = pack(&[(22, 0x6F), (8, 5), (2, 1), (1, 0), (1, 0)]);
        let h = parse_frame_header(&bytes).expect("valid P-frame header");
        assert_eq!(h.picture_type, Svq1PictureType::Predicted);
        assert_eq!(h.temporal_reference, 5);
        assert!(h.frame_size_code.is_none());
        assert!(h.width.is_none());
        assert!(h.height.is_none());
        // 22 + 8 + 2 + 1 + 1 = 34
        assert_eq!(h.header_end_bit, 34);
    }

    /// B-frame ("droppable"): same shape as P-frame, distinct enum
    /// value.
    #[test]
    fn bframe_is_droppable() {
        let bytes = pack(&[(22, 0x6F), (8, 1), (2, 2), (1, 0), (1, 0)]);
        let h = parse_frame_header(&bytes).expect("valid B-frame header");
        assert_eq!(h.picture_type, Svq1PictureType::Droppable);
    }

    /// Invalid frame code: bits 6-5 both clear ⇒
    /// `Error::InvalidFrameCode`.
    #[test]
    fn invalid_frame_code_bits_65_clear() {
        // 0x80 has bit 7 set but bits 6 & 5 clear → invalid.
        let bytes = pack(&[(22, 0x80), (8, 0), (2, 0)]);
        let err = parse_frame_header(&bytes).expect_err("bits 6-5 both clear must reject");
        assert!(matches!(err, Error::InvalidFrameCode(0x80)));
    }

    /// Picture-type value 3 ⇒ `Error::InvalidPictureType`.
    #[test]
    fn picture_type_3_rejected() {
        let bytes = pack(&[(22, 0x6F), (8, 0), (2, 3)]);
        let err = parse_frame_header(&bytes).expect_err("picture type 3 must reject");
        assert!(matches!(err, Error::InvalidPictureType));
    }

    /// Non-zero `unknown_field_1` in the checksum trailer ⇒
    /// `Error::InvalidChecksumTrailer`.
    #[test]
    fn checksum_trailer_reserved_must_be_zero() {
        // P-frame, checksum_present=1, use_packet=0, component=0,
        // reserved2=1 (forbidden).
        let bytes = pack(&[(22, 0x6F), (8, 0), (2, 1), (1, 1), (1, 0), (1, 0), (2, 1)]);
        let err = parse_frame_header(&bytes).expect_err("non-zero reserved2 must reject");
        assert!(matches!(err, Error::InvalidChecksumTrailer));
    }

    /// Checksum trailer with all zero reserved bits is accepted and
    /// captured.
    #[test]
    fn checksum_trailer_accepted_when_reserved_zero() {
        let bytes = pack(&[
            (22, 0x6F),
            (8, 0),
            (2, 1),
            (1, 1),
            (1, 1),
            (1, 0),
            (2, 0),
            (1, 0),
        ]);
        let h = parse_frame_header(&bytes).expect("valid trailer");
        let t = h.checksum_trailer.expect("trailer present");
        assert!(t.use_packet_checksum);
        assert!(!t.component_checksums);
    }

    /// `unknown_flag_1` set ⇒ 1+4+1+2 fixed bits are captured and
    /// the byte-loop tail accumulates while the peek-bit is 1.
    #[test]
    fn unknown_flag_1_loop_captures_bytes() {
        // P-frame, no checksum trailer, unknown_flag_1=1.
        // Fixed 8 bits: d=1, e=0xA, f=0, g=2 → packed
        // 1 1010 0 10 → 0b1_1010_010 = 0xD2.
        // Then peek-loop: 1 0xFF, 1 0xAA, 0 (stop).
        let bytes = pack(&[
            (22, 0x6F),
            (8, 0),
            (2, 1),
            (1, 0), // checksum_present=0
            (1, 1), // unknown_flag_1=1
            (1, 1),
            (4, 0xA),
            (1, 0),
            (2, 2),
            (1, 1),
            (8, 0xFF),
            (1, 1),
            (8, 0xAA),
            (1, 0),
        ]);
        let h = parse_frame_header(&bytes).expect("valid header with unknown_flag_1");
        assert_eq!(h.unknown_flag_1_payload, Some(0xD2));
        assert_eq!(h.unknown_flag_1_extras, vec![0xFF, 0xAA]);
    }

    /// Truncated mid-header returns `Error::Truncated`.
    #[test]
    fn truncated_during_frame_code() {
        // Only 2 bytes (16 bits) when the frame code alone needs 22.
        let err = parse_frame_header(&[0xFF, 0xFF]).expect_err("truncated frame code must reject");
        assert!(matches!(err, Error::Truncated));
    }

    /// `is_intra` predicate aligns with the picture-type enum.
    /// I-frame uses `frame_code = 0x40` to keep all optional
    /// sub-fields suppressed.
    #[test]
    fn is_intra_predicate() {
        let bytes = pack(&[
            (22, 0x40),
            (8, 0),
            (2, 0),
            (2, 0),
            (2, 0),
            (1, 0),
            (3, 0),
            (1, 0),
            (1, 0),
        ]);
        let h = parse_frame_header(&bytes).unwrap();
        assert!(h.is_intra());

        let pbytes = pack(&[(22, 0x6F), (8, 0), (2, 1), (1, 0), (1, 0)]);
        let hp = parse_frame_header(&pbytes).unwrap();
        assert!(!hp.is_intra());
    }

    /// Frame-size-code lookup-table sanity: every code 0..=6 maps to
    /// a documented dimension pair. Uses `frame_code = 0x40` per the
    /// rationale on [`iframe_size_code_2_minimal`].
    #[test]
    fn frame_size_table_exhaustive() {
        let expected: &[(u8, u16, u16)] = &[
            (0, 160, 120),
            (1, 128, 96),
            (2, 176, 144),
            (3, 352, 288),
            (4, 704, 576),
            (5, 240, 180),
            (6, 320, 240),
        ];
        for &(code, w, h) in expected {
            let bytes = pack(&[
                (22, 0x40),
                (8, 0),
                (2, 0),
                (2, 0),
                (2, 0),
                (1, 0),
                (3, code as u32),
                (1, 0),
                (1, 0),
            ]);
            let hdr = parse_frame_header(&bytes).expect("valid header");
            assert_eq!(hdr.width, Some(w), "code {code} width");
            assert_eq!(hdr.height, Some(h), "code {code} height");
        }
    }
}
