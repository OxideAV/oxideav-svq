//! SVQ3 stream header, access-unit envelope, slice header and the
//! universal variable-length code.
//!
//! Every statement here is anchored in the clean-room staging under
//! `docs/video/svq3/`:
//!
//! * the `SEQH` stream header — `spec/02-seqh-extradata.md` (§2 the
//!   size fields and the fixed size table, §3 the ten-bit flag group
//!   with its two motion-precision enables, the post-filter hint, the
//!   extended-mode flag, the B-frame / output-delay bit, the escape
//!   for further data bytes and the protected flag);
//! * the access-unit envelope — `spec/07-slice-and-macroblock-layer.md`
//!   §2 (packet byte, length field of `L` bytes, the payload addressed
//!   from packet byte 2 with the displaced bytes relocated to its end,
//!   the `0xff` end marker);
//! * the slice header — `spec/07` §3.1 (packet type 1), §3.2 (packet
//!   type 2: `first_mb` instead of the `encrypted` flag) and §3.3 (the
//!   extended-mode read sequence, parsed but not interpreted);
//! * the universal code — `spec/06-residual-coefficient-coding.md` §1
//!   / `spec/07` §1: `2n + 1` bits, a marker bit *before* every data
//!   bit and a closing `1` (`0 d₁ 0 d₂ … 0 dₙ 1`), code number
//!   `2ⁿ − 1 + value`.
//!
//! The macroblock layer that follows the header is [`crate::svq3_frame`].

use crate::bitreader::BitReader;
use crate::error::{Error, Result};

/// SVQ3 sequence-header marker bytes (`"SEQH"`), spec/02 §1: the
/// cookie is `u32be size`, the tag, then `size` payload bytes; in
/// QuickTime it is nested inside an `SMI ` box (§4).
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub const SVQ3_SEQH_MAGIC: [u8; 4] = *b"SEQH";

/// The access-unit end marker: a packet byte of `0xff` ends the
/// access unit (spec/07 §2).
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub const SVQ3_FRAME_END: u8 = 0xFF;

/// The fixed coded-size table selected by `frame_size_code` 0…6
/// (spec/02 §2). Code 7 means the explicit 12-bit width and height
/// follow. Slot 5 is the codec's own `1152 × 1408` comparison value —
/// distinct from the SVQ1 size table, which is why this is not shared
/// with [`crate::header::FRAME_SIZE_TABLE`].
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub const SVQ3_FRAME_SIZE_TABLE: [(u16, u16); 7] = [
    (160, 120),
    (128, 96),
    (176, 144),
    (352, 288),
    (704, 576),
    (1152, 1408),
    (320, 240),
];

/// The parsed `SEQH` stream header (spec/02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svq3SequenceHeader {
    /// `u(3) frame_size_code`; 7 = explicit dimensions follow.
    pub frame_size_code: u8,
    /// Coded picture width in luma samples.
    pub width: u16,
    /// Coded picture height in luma samples.
    pub height: u16,
    /// Flag-group bit 1: half-sample motion vectors may occur
    /// (spec/05 §1).
    pub has_halfpel: bool,
    /// Flag-group bit 2: third-sample motion vectors may occur
    /// (spec/05 §1).
    pub has_thirdpel: bool,
    /// Flag-group bit 3: a size-gated decoder-side post-processing
    /// option (spec/02 §3.1) — not wire syntax beyond the bit.
    pub postfilter_hint: bool,
    /// Flag-group bit 4: the extended macroblock-layer mode
    /// (spec/02 §3.2); every slice header must echo it in its `mode`
    /// bit.
    pub extended_mode: bool,
    /// Flag-group bits 5 and 6, written as the constants `1`, `1`
    /// (bit 5 in the high position).
    pub reserved_5_6: u8,
    /// Flag-group bit 7: `true` = the stream carries no B-frames and
    /// the decoder outputs without delay; `false` = a three-picture
    /// pool with a one-access-unit output delay (spec/02 §3.3).
    pub no_b_frames: bool,
    /// Flag-group bit 8, written as the constant `0`.
    pub reserved_8: bool,
    /// The further 8-bit data bytes introduced by the bit-9 escape
    /// (`while u(1): u(8)`); every observed stream carries none.
    pub optional_bytes: Vec<u8>,
    /// Flag-group bit 10: the stream is watermark-protected. The
    /// protection payload is not specified and is not parsed.
    pub protected: bool,
    /// Number of payload bits consumed by the parse.
    pub header_end_bit: usize,
}

/// The two slice packet types of spec/07 §2 / §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceVersion {
    /// Packet type 1: the header carries the `encrypted` flag and the
    /// slice starts at macroblock 0 (§3.1).
    V1,
    /// Packet type 2: the header carries `first_mb`, the raster index
    /// of the slice's first macroblock (§3.2).
    V2,
}

/// The slice type (spec/07 §3.1 `slice_type`: 0 = P, 1 = B, 2 = I).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq3FrameType {
    /// `slice_type` 0.
    Predicted,
    /// `slice_type` 1.
    Bidirectional,
    /// `slice_type` 2.
    Intra,
}

impl Svq3FrameType {
    /// Map the `slice_type` code number to the typed value; any other
    /// code number is [`Error::InvalidFrameCode`].
    pub fn from_code(code: u32) -> Result<Self> {
        match code {
            0 => Ok(Self::Predicted),
            1 => Ok(Self::Bidirectional),
            2 => Ok(Self::Intra),
            other => Err(Error::InvalidFrameCode(other)),
        }
    }
}

/// The extended-mode slice-header fields of spec/07 §3.3, read in the
/// decoder's sequence when the header's `mode` bit is 1. Their meaning
/// is not established; the names are the chapter's placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svq3ExtendedModeFields {
    /// `u(2) a`; a value of 3 is a stream error.
    pub a: u8,
    /// `u(2) b`.
    pub b: u8,
    /// `u(3) c`.
    pub c: u8,
    /// `u(3) d`, present when `c ∈ {1, 3}`.
    pub d: Option<u8>,
    /// `u(8) e`, present when `c ∈ {1, 4}`.
    pub e: Option<u8>,
    /// `u(1) f` (read after the extension bytes).
    pub f: bool,
    /// `u(8) g`, present when `f = 1`.
    pub g: Option<u8>,
    /// `u(3) h`, present when `c ∈ {4, 5}`.
    pub h: Option<u8>,
    /// The four `uvlc` values `r0 … r3`, raw (the chapter records the
    /// stored forms `16·r0`, `16·r1`, `16·r2 + 16·r0 − 1`,
    /// `16·r3 + 16·r1 − 1` — a macroblock-aligned rectangle, inferred).
    pub r: [u32; 4],
    /// The trailing `u(1)`.
    pub tail1: bool,
    /// The trailing `u(2)`.
    pub tail2: u8,
}

/// A parsed slice header (spec/07 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svq3SliceHeader {
    /// Packet type (1 or 2).
    pub version: SliceVersion,
    /// `L`, the width of the packet's length field in bytes (1…3).
    pub slice_size_size: u8,
    /// The payload length in bytes.
    pub slice_size: u32,
    /// `uvlc slice_type`.
    pub frame_type: Svq3FrameType,
    /// Packet type 1 only: `u(1) encrypted` (must be 0; the encrypted
    /// path is not specified).
    pub encrypted: Option<bool>,
    /// Packet type 2 only: `u(N) first_mb`,
    /// `N = max(6, ⌈log₂(mb_count + 1)⌉)`.
    pub first_mb: Option<u32>,
    /// `u(8) picture_id` — frame counter modulo 256.
    pub picture_id: u8,
    /// `u(5) qp` — the slice quantiser.
    pub slice_qp: u8,
    /// `u(1) mb_qp_delta_enable`.
    pub mb_qp_delta_enable: bool,
    /// The `u(1)` flag whose meaning is not established (stored, not
    /// read by the macroblock layer).
    pub flag: bool,
    /// The `u(1)` present only for protected streams; not specified.
    pub protected_flag: Option<bool>,
    /// `u(1) mode` — must equal the `SEQH` `extended_mode` flag.
    pub mode: bool,
    /// The reserved `u(2)`.
    pub reserved: u8,
    /// The `while u(1): u(8)` extension bytes.
    pub extension_bytes: Vec<u8>,
    /// The §3.3 fields when `mode = 1`.
    pub extended: Option<Svq3ExtendedModeFields>,
    /// Bit position (within the payload) of the first macroblock.
    pub header_end_bit: usize,
}

impl Svq3SliceHeader {
    /// `true` when this slice belongs to an I picture.
    pub fn is_intra(&self) -> bool {
        self.frame_type == Svq3FrameType::Intra
    }
}

/// Macroblock count of the picture (`⌈w/16⌉ · ⌈h/16⌉`).
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn num_macroblocks(seqh: &Svq3SequenceHeader) -> u32 {
    let (mb_w, mb_h) = mb_grid_dims(seqh);
    mb_w * mb_h
}

/// Macroblock grid `(columns, rows)` of the picture.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn mb_grid_dims(seqh: &Svq3SequenceHeader) -> (u32, u32) {
    let mb_w = (seqh.width as u32).div_ceil(16);
    let mb_h = (seqh.height as u32).div_ceil(16);
    (mb_w, mb_h)
}

/// A macroblock's grid position plus whether a macroblock exists above
/// / to its left in the picture (spec/07 §10.1's "missing" notion).
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Svq3MacroblockPosition {
    /// Column in macroblocks.
    pub mb_x: u32,
    /// Row in macroblocks.
    pub mb_y: u32,
    /// A macroblock row exists above (`mb_y > 0`).
    pub top_available: bool,
    /// A macroblock column exists to the left (`mb_x > 0`).
    pub left_available: bool,
}

impl Svq3MacroblockPosition {
    /// The macroblock's top-left luma sample.
    pub fn luma_origin(&self) -> (u32, u32) {
        (self.mb_x * 16, self.mb_y * 16)
    }
}

/// Resolve raster index `mb_index` on a `mb_cols`-wide grid.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn macroblock_position(mb_index: u32, mb_cols: u32) -> Result<Svq3MacroblockPosition> {
    if mb_cols == 0 {
        return Err(Error::BadBitWidth(0));
    }
    let mb_x = mb_index % mb_cols;
    let mb_y = mb_index / mb_cols;
    Ok(Svq3MacroblockPosition {
        mb_x,
        mb_y,
        top_available: mb_y > 0,
        left_available: mb_x > 0,
    })
}

/// `⌈log₂ value⌉` (0 for `value ≤ 1`).
fn ceil_log2(value: u32) -> u32 {
    if value <= 1 {
        return 0;
    }
    32 - (value - 1).leading_zeros()
}

/// Width of the packet-type-2 `first_mb` field for a picture of
/// `mb_count` macroblocks: `max(6, ⌈log₂(mb_count + 1)⌉)` (spec/07
/// §3.2).
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn first_mb_field_width(mb_count: u32) -> u32 {
    ceil_log2(mb_count.saturating_add(1)).max(6)
}

/// Strip the `"SEQH"` tag + `u32be size` prefix, returning the
/// declared payload (spec/02 §1). Callers that hold the QuickTime
/// `SMI ` wrapper locate the tag first
/// ([`crate::registry::make_svq3_decoder`] does).
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn strip_seqh_prefix(extradata: &[u8]) -> Result<&[u8]> {
    if extradata.len() < 8 {
        return Err(Error::Truncated);
    }
    if extradata[0..4] != SVQ3_SEQH_MAGIC {
        return Err(Error::InvalidFrameCode(u32::from_be_bytes([
            extradata[0],
            extradata[1],
            extradata[2],
            extradata[3],
        ])));
    }
    let declared = u32::from_be_bytes([extradata[4], extradata[5], extradata[6], extradata[7]]);
    let end = 8usize
        .checked_add(declared as usize)
        .ok_or(Error::Truncated)?;
    if extradata.len() < end {
        return Err(Error::Truncated);
    }
    Ok(&extradata[8..end])
}

/// Parse the `SEQH` payload (spec/02 §2 + §3).
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn parse_sequence_header(payload: &[u8]) -> Result<Svq3SequenceHeader> {
    let mut br = BitReader::new(payload);

    let frame_size_code = br.read_bits(3)? as u8;
    let (width, height) = match frame_size_code {
        7 => {
            let w = br.read_bits(12)? as u16;
            let h = br.read_bits(12)? as u16;
            (w, h)
        }
        other => SVQ3_FRAME_SIZE_TABLE[other as usize],
    };

    // The ten-bit flag group of spec/02 §3.
    let has_halfpel = br.read_bit()? == 1;
    let has_thirdpel = br.read_bit()? == 1;
    let postfilter_hint = br.read_bit()? == 1;
    let extended_mode = br.read_bit()? == 1;
    let reserved_5_6 = br.read_bits(2)? as u8;
    let no_b_frames = br.read_bit()? == 1;
    let reserved_8 = br.read_bit()? == 1;
    // Bit 9 is the escape: a 1 introduces one more byte and the escape
    // repeats after it.
    let mut optional_bytes = Vec::new();
    while br.read_bit()? == 1 {
        optional_bytes.push(br.read_bits(8)? as u8);
    }
    let protected = br.read_bit()? == 1;

    Ok(Svq3SequenceHeader {
        frame_size_code,
        width,
        height,
        has_halfpel,
        has_thirdpel,
        postfilter_hint,
        extended_mode,
        reserved_5_6,
        no_b_frames,
        reserved_8,
        optional_bytes,
        protected,
        header_end_bit: br.bits_consumed(),
    })
}

/// [`strip_seqh_prefix`] then [`parse_sequence_header`].
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn parse_extradata(extradata: &[u8]) -> Result<Svq3SequenceHeader> {
    let payload = strip_seqh_prefix(extradata)?;
    parse_sequence_header(payload)
}

/// Locate the `SEQH` block inside container-flavoured extradata (the
/// QuickTime `SMI ` wrapper of spec/02 §4, or bare `SEQH` bytes) and
/// parse it. Returns [`Error::InvalidFrameCode`] when no tag is found.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn parse_extradata_flexible(extradata: &[u8]) -> Result<Svq3SequenceHeader> {
    let start = extradata
        .windows(SVQ3_SEQH_MAGIC.len())
        .position(|w| w == SVQ3_SEQH_MAGIC)
        .ok_or(Error::InvalidFrameCode(0))?;
    parse_extradata(&extradata[start..])
}

/// Undo the envelope's byte relocation (spec/07 §2): the payload is
/// `packet[length+2 : length+1+L] ++ packet[1+L : length+2]`, i.e.
/// the last `L − 1` bytes of the on-wire body move to the front.
/// `body` is the `length` bytes following the packet byte and the
/// length field.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn unpermute_slice_payload(body: &[u8], slice_size_size: u8) -> Result<Vec<u8>> {
    if !(1..=3).contains(&slice_size_size) {
        return Err(Error::BadBitWidth(slice_size_size as u32));
    }
    let moved = (slice_size_size - 1) as usize;
    if body.len() < moved {
        return Err(Error::Truncated);
    }
    if moved == 0 {
        return Ok(body.to_vec());
    }
    let (head, tail) = body.split_at(body.len() - moved);
    let mut out = Vec::with_capacity(body.len());
    out.extend_from_slice(tail);
    out.extend_from_slice(head);
    Ok(out)
}

/// Read one code number of the universal code (spec/06 §1 / spec/07
/// §1): a leading `1` is code number 0; otherwise the code is
/// `0 d₁ 0 d₂ … 0 dₙ 1` — a marker bit before every data bit, ending
/// at the first marker that is 1 — and the code number is
/// `2ⁿ − 1 + value`.
///
/// More than 31 data bits cannot be represented and is reported as
/// [`Error::BadBitWidth`].
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn read_universal_code(br: &mut BitReader<'_>) -> Result<u32> {
    if br.read_bit()? == 1 {
        return Ok(0);
    }
    let mut n: u32 = 0;
    let mut value: u32 = 0;
    loop {
        // The marker before this data bit was 0; read the data bit.
        value = (value << 1) | br.read_bit()? as u32;
        n += 1;
        if br.read_bit()? == 1 {
            break;
        }
        if n >= 31 {
            return Err(Error::BadBitWidth(n + 1));
        }
    }
    Ok((1u32 << n) - 1 + value)
}

/// The §3.3 fields read between `mode` and the reserved bits.
struct ExtendedGroup1 {
    a: u8,
    b: u8,
    c: u8,
    d: Option<u8>,
    e: Option<u8>,
}

/// Read the §3.3 first group (after `mode`, before the reserved bits).
fn read_extended_group_1(br: &mut BitReader<'_>) -> Result<ExtendedGroup1> {
    let a = br.read_bits(2)? as u8;
    if a >= 3 {
        return Err(Error::InvalidFrameCode(a as u32));
    }
    let b = br.read_bits(2)? as u8;
    let c = br.read_bits(3)? as u8;
    let d = if c == 1 || c == 3 {
        Some(br.read_bits(3)? as u8)
    } else {
        None
    };
    let e = if c == 1 || c == 4 {
        Some(br.read_bits(8)? as u8)
    } else {
        None
    };
    Ok(ExtendedGroup1 { a, b, c, d, e })
}

/// Parse a slice header from the start of the unpermuted payload
/// (spec/07 §3.1–§3.3).
///
/// `version` / `slice_size_size` / `slice_size` come from the packet
/// byte and length field; `num_mbs` sizes the packet-type-2
/// `first_mb` field; `protected` and `extended_mode` are the `SEQH`
/// flags that add the protected-only bit and the §3.3 fields. A
/// `mode` bit that disagrees with `extended_mode` is a stream error
/// (spec/02 §3.2).
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn parse_slice_header(
    unpermuted_body: &[u8],
    version: SliceVersion,
    slice_size_size: u8,
    slice_size: u32,
    num_mbs: u32,
    protected: bool,
    extended_mode: bool,
) -> Result<Svq3SliceHeader> {
    let mut br = BitReader::new(unpermuted_body);

    let frame_type = Svq3FrameType::from_code(read_universal_code(&mut br)?)?;

    let (encrypted, first_mb) = match version {
        SliceVersion::V1 => (Some(br.read_bit()? == 1), None),
        SliceVersion::V2 => (None, Some(br.read_bits(first_mb_field_width(num_mbs))?)),
    };

    let picture_id = br.read_bits(8)? as u8;
    let slice_qp = br.read_bits(5)? as u8;
    let mb_qp_delta_enable = br.read_bit()? == 1;
    let flag = br.read_bit()? == 1;
    let protected_flag = if protected {
        Some(br.read_bit()? == 1)
    } else {
        None
    };
    let mode = br.read_bit()? == 1;
    if mode != extended_mode {
        return Err(Error::InvalidFrameCode(mode as u32));
    }
    let group_1 = if mode {
        Some(read_extended_group_1(&mut br)?)
    } else {
        None
    };
    let reserved = br.read_bits(2)? as u8;

    let mut extension_bytes = Vec::new();
    while br.read_bit()? == 1 {
        extension_bytes.push(br.read_bits(8)? as u8);
    }

    let extended = match group_1 {
        None => None,
        Some(ExtendedGroup1 { a, b, c, d, e }) => {
            let f = br.read_bit()? == 1;
            let g = if f {
                Some(br.read_bits(8)? as u8)
            } else {
                None
            };
            let h = if c == 4 || c == 5 {
                Some(br.read_bits(3)? as u8)
            } else {
                None
            };
            let mut r = [0u32; 4];
            for slot in r.iter_mut() {
                *slot = read_universal_code(&mut br)?;
            }
            let tail1 = br.read_bit()? == 1;
            let tail2 = br.read_bits(2)? as u8;
            Some(Svq3ExtendedModeFields {
                a,
                b,
                c,
                d,
                e,
                f,
                g,
                h,
                r,
                tail1,
                tail2,
            })
        }
    };

    Ok(Svq3SliceHeader {
        version,
        slice_size_size,
        slice_size,
        frame_type,
        encrypted,
        first_mb,
        picture_id,
        slice_qp,
        mb_qp_delta_enable,
        flag,
        protected_flag,
        mode,
        reserved,
        extension_bytes,
        extended,
        header_end_bit: br.bits_consumed(),
    })
}

/// The decoded packet byte of spec/07 §2.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq3PacketKind {
    /// The `0xff` end-of-access-unit marker.
    End,
    /// Packet type 0: followed by a `u(16)` that must be zero.
    Zero {
        /// `L`, the width of the length field.
        length_width: u8,
    },
    /// Packet type 1 or 2: a slice.
    Slice {
        /// Which slice packet type.
        version: SliceVersion,
        /// `L`, the width of the length field (1…3 for a slice).
        length_width: u8,
    },
}

/// Classify a packet byte (spec/07 §2): `0xff` ends the access unit;
/// otherwise `L = (b >> 5) & 3` and `T = b & 0x9f`, where any type
/// with bit 7 set is an error, types 1 and 2 are slices and type 0 is
/// the zero packet. A slice with `L = 0` has no length field and is
/// rejected.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn classify_packet_byte(b: u8) -> Result<Svq3PacketKind> {
    if b == SVQ3_FRAME_END {
        return Ok(Svq3PacketKind::End);
    }
    if b & 0x80 != 0 {
        return Err(Error::InvalidFrameCode(b as u32));
    }
    let length_width = (b >> 5) & 3;
    match b & 0x1f {
        0 => Ok(Svq3PacketKind::Zero { length_width }),
        1 | 2 if length_width == 0 => Err(Error::BadBitWidth(0)),
        1 => Ok(Svq3PacketKind::Slice {
            version: SliceVersion::V1,
            length_width,
        }),
        2 => Ok(Svq3PacketKind::Slice {
            version: SliceVersion::V2,
            length_width,
        }),
        other => Err(Error::InvalidFrameCode(other as u32)),
    }
}

/// A slice packet split into its parsed header and its unpermuted
/// payload, plus where the next packet starts.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svq3WireSlice {
    /// The parsed header.
    pub header: Svq3SliceHeader,
    /// The unpermuted payload (`length` bytes); the macroblock layer
    /// starts at bit `header.header_end_bit`.
    pub payload: Vec<u8>,
    /// Bytes consumed from the wire, i.e. the offset of the next
    /// packet byte.
    pub consumed: usize,
}

/// Read one slice packet (spec/07 §2 + §3) from `wire`, which must
/// start at the packet byte. Returns the header, the unpermuted
/// payload and the packet's total byte length.
///
/// A leading end marker or a non-slice packet byte is rejected
/// ([`Error::Truncated`] for the marker, so callers that classify with
/// [`classify_packet_byte`] first never see it; the registry probe
/// relies on the type/width checks).
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn parse_wire_slice(
    wire: &[u8],
    num_mbs: u32,
    protected: bool,
    extended_mode: bool,
) -> Result<Svq3WireSlice> {
    if wire.is_empty() {
        return Err(Error::Truncated);
    }
    let (version, length_width) = match classify_packet_byte(wire[0])? {
        Svq3PacketKind::Slice {
            version,
            length_width,
        } => (version, length_width),
        Svq3PacketKind::End => return Err(Error::Truncated),
        Svq3PacketKind::Zero { .. } => return Err(Error::InvalidFrameCode(0)),
    };
    let want = 1 + length_width as usize;
    if wire.len() < want {
        return Err(Error::Truncated);
    }
    let mut slice_size: u32 = 0;
    for &b in &wire[1..want] {
        slice_size = (slice_size << 8) | b as u32;
    }
    let body_end = want
        .checked_add(slice_size as usize)
        .ok_or(Error::Truncated)?;
    if wire.len() < body_end {
        return Err(Error::Truncated);
    }
    let payload = unpermute_slice_payload(&wire[want..body_end], length_width)?;
    let header = parse_slice_header(
        &payload,
        version,
        length_width,
        slice_size,
        num_mbs,
        protected,
        extended_mode,
    )?;
    Ok(Svq3WireSlice {
        header,
        payload,
        consumed: body_end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svq3_testutil::{pack, uvlc, Packer};

    #[test]
    fn ceil_log2_known_values() {
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(8), 3);
        assert_eq!(ceil_log2(9), 4);
        assert_eq!(ceil_log2(17), 5);
    }

    #[test]
    fn first_mb_width_is_at_least_six() {
        // spec/07 §3.2: N = max(6, ⌈log₂(mb_count + 1)⌉).
        assert_eq!(first_mb_field_width(4), 6);
        assert_eq!(first_mb_field_width(63), 6);
        assert_eq!(first_mb_field_width(64), 7);
        assert_eq!(first_mb_field_width(120), 7);
        assert_eq!(first_mb_field_width(300), 9);
        assert_eq!(first_mb_field_width(511), 9);
        assert_eq!(first_mb_field_width(512), 10);
    }

    #[test]
    fn universal_code_first_codewords_match_spec07_table() {
        // spec/07 §1: 0 `1`, 1 `001`, 2 `011`, 3 `00001`, 4 `00011`,
        // 5 `01001`, 6 `01011`, 7 `0000001`.
        let cases: &[(&[u8], u32)] = &[
            (&[0b1000_0000], 0),
            (&[0b0010_0000], 1),
            (&[0b0110_0000], 2),
            (&[0b0000_1000], 3),
            (&[0b0001_1000], 4),
            (&[0b0100_1000], 5),
            (&[0b0101_1000], 6),
            (&[0b0000_0010], 7),
            (&[0b0000_0110], 8),
            (&[0b0001_0010], 9),
        ];
        for &(bytes, expected) in cases {
            let mut br = BitReader::new(bytes);
            assert_eq!(read_universal_code(&mut br).unwrap(), expected, "{bytes:?}");
        }
    }

    #[test]
    fn universal_code_encoder_decoder_agree() {
        for code in 0..=4000u32 {
            let (w, bits) = uvlc(code);
            let bytes = pack(&[(w, bits)]);
            let mut br = BitReader::new(&bytes);
            assert_eq!(read_universal_code(&mut br).unwrap(), code);
            assert_eq!(br.bits_consumed() as u32, w);
            // Length is 2n + 1 with n = ⌊log₂(code + 1)⌋.
            let n = 31 - (code + 1).leading_zeros();
            assert_eq!(w, 2 * n + 1);
        }
    }

    #[test]
    fn universal_code_worked_example_code_29_and_608() {
        // spec/07 §11: the pattern code `010101001` → 29; the escape
        // code number 608 is 19 bits long.
        let bytes = pack(&[(9, 0b0_1010_1001)]);
        let mut br = BitReader::new(&bytes);
        assert_eq!(read_universal_code(&mut br).unwrap(), 29);
        assert_eq!(uvlc(608).0, 19);
    }

    #[test]
    fn universal_code_truncated_input_errors() {
        let mut br = BitReader::new(&[0b0000_0000]);
        assert_eq!(read_universal_code(&mut br).unwrap_err(), Error::Truncated);
        let mut br = BitReader::new(&[]);
        assert_eq!(read_universal_code(&mut br).unwrap_err(), Error::Truncated);
    }

    #[test]
    fn universal_code_rejects_over_long_codes() {
        // 31 data bits are representable; a 32nd is not.
        let zeros = [0u8; 12];
        let mut br = BitReader::new(&zeros);
        assert!(matches!(
            read_universal_code(&mut br),
            Err(Error::BadBitWidth(_)) | Err(Error::Truncated)
        ));
    }

    #[test]
    fn frame_type_code_mapping() {
        assert_eq!(
            Svq3FrameType::from_code(0).unwrap(),
            Svq3FrameType::Predicted
        );
        assert_eq!(
            Svq3FrameType::from_code(1).unwrap(),
            Svq3FrameType::Bidirectional
        );
        assert_eq!(Svq3FrameType::from_code(2).unwrap(), Svq3FrameType::Intra);
        assert_eq!(
            Svq3FrameType::from_code(3).unwrap_err(),
            Error::InvalidFrameCode(3)
        );
    }

    fn seqh_bytes(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&SVQ3_SEQH_MAGIC);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn strip_seqh_prefix_checks() {
        assert_eq!(strip_seqh_prefix(&[]).unwrap_err(), Error::Truncated);
        assert!(matches!(
            strip_seqh_prefix(b"XXXX\0\0\0\0"),
            Err(Error::InvalidFrameCode(_))
        ));
        assert_eq!(
            strip_seqh_prefix(&seqh_bytes(&[1, 2, 3])).unwrap(),
            &[1, 2, 3]
        );
        assert_eq!(
            strip_seqh_prefix(b"SEQH\0\0\0\x05\x01").unwrap_err(),
            Error::Truncated
        );
    }

    #[test]
    fn parse_seqh_fixture_240x128() {
        // fixtures/real-sample-240x128: payload e1 e0 10 19 c0 →
        // code 7, 240 × 128, flag group 1100111000.
        let h = parse_sequence_header(&[0xe1, 0xe0, 0x10, 0x19, 0xc0]).unwrap();
        assert_eq!(h.frame_size_code, 7);
        assert_eq!((h.width, h.height), (240, 128));
        assert!(h.has_halfpel);
        assert!(h.has_thirdpel);
        assert!(!h.postfilter_hint);
        assert!(!h.extended_mode);
        assert_eq!(h.reserved_5_6, 0b11);
        assert!(h.no_b_frames);
        assert!(!h.reserved_8);
        assert!(h.optional_bytes.is_empty());
        assert!(!h.protected);
        assert_eq!(h.header_end_bit, 37);
    }

    #[test]
    fn parse_seqh_fixture_320x240_short() {
        // fixtures/real-sample-320x240-short-seqh: payload d5 80 →
        // code 6 (320 × 240), flag group 1010110000.
        let h = parse_sequence_header(&[0xd5, 0x80]).unwrap();
        assert_eq!(h.frame_size_code, 6);
        assert_eq!((h.width, h.height), (320, 240));
        assert!(h.has_halfpel);
        assert!(!h.has_thirdpel);
        assert!(h.postfilter_hint);
        assert!(!h.extended_mode);
        assert_eq!(h.reserved_5_6, 0b11);
        assert!(!h.no_b_frames);
        assert!(!h.protected);
        assert_eq!(h.header_end_bit, 13);
    }

    #[test]
    fn parse_seqh_escape_bytes_and_protected() {
        let mut p = Packer::new();
        p.push(3, 2); // 176 × 144
        p.push(1, 1);
        p.push(1, 0);
        p.push(1, 0);
        p.push(1, 1); // extended mode
        p.push(2, 0b11);
        p.push(1, 1);
        p.push(1, 0);
        p.push(1, 1); // escape
        p.push(8, 0xab);
        p.push(1, 1); // escape again
        p.push(8, 0xcd);
        p.push(1, 0);
        p.push(1, 1); // protected
        let h = parse_sequence_header(&p.into_bytes()).unwrap();
        assert_eq!((h.width, h.height), (176, 144));
        assert!(h.extended_mode);
        assert_eq!(h.optional_bytes, vec![0xab, 0xcd]);
        assert!(h.protected);
    }

    #[test]
    fn parse_seqh_size_table_slot_5_is_the_svq3_value() {
        let h = parse_sequence_header(&[0b1011_0011, 0b1000_0000]).unwrap();
        assert_eq!(h.frame_size_code, 5);
        assert_eq!((h.width, h.height), (1152, 1408));
    }

    #[test]
    fn parse_extradata_wraps_and_parses() {
        let h = parse_extradata(&seqh_bytes(&[0xd5, 0x80])).unwrap();
        assert_eq!((h.width, h.height), (320, 240));
    }

    #[test]
    fn grid_helpers() {
        let mut h = parse_sequence_header(&[0xd5, 0x80]).unwrap();
        assert_eq!(mb_grid_dims(&h), (20, 15));
        assert_eq!(num_macroblocks(&h), 300);
        h.width = 470;
        h.height = 352;
        assert_eq!(mb_grid_dims(&h), (30, 22));
        let p = macroblock_position(21, 20).unwrap();
        assert_eq!((p.mb_x, p.mb_y), (1, 1));
        assert!(p.top_available && p.left_available);
        assert_eq!(p.luma_origin(), (16, 16));
        let p = macroblock_position(19, 20).unwrap();
        assert!(!p.top_available && p.left_available);
        assert_eq!(
            macroblock_position(0, 0).unwrap_err(),
            Error::BadBitWidth(0)
        );
    }

    #[test]
    fn unpermute_moves_trailing_bytes_to_front() {
        assert_eq!(
            unpermute_slice_payload(&[1, 2, 3, 4], 1).unwrap(),
            [1, 2, 3, 4]
        );
        assert_eq!(
            unpermute_slice_payload(&[1, 2, 3, 4], 2).unwrap(),
            [4, 1, 2, 3]
        );
        assert_eq!(
            unpermute_slice_payload(&[1, 2, 3, 4], 3).unwrap(),
            [3, 4, 1, 2]
        );
        assert_eq!(
            unpermute_slice_payload(&[1], 3).unwrap_err(),
            Error::Truncated
        );
        assert_eq!(
            unpermute_slice_payload(&[1], 0).unwrap_err(),
            Error::BadBitWidth(0)
        );
    }

    #[test]
    fn classify_packet_bytes() {
        assert_eq!(classify_packet_byte(0xff).unwrap(), Svq3PacketKind::End);
        assert_eq!(
            classify_packet_byte(0x41).unwrap(),
            Svq3PacketKind::Slice {
                version: SliceVersion::V1,
                length_width: 2
            }
        );
        assert_eq!(
            classify_packet_byte(0x22).unwrap(),
            Svq3PacketKind::Slice {
                version: SliceVersion::V2,
                length_width: 1
            }
        );
        assert_eq!(
            classify_packet_byte(0x20).unwrap(),
            Svq3PacketKind::Zero { length_width: 1 }
        );
        assert!(classify_packet_byte(0x81).is_err());
        assert!(classify_packet_byte(0x01).is_err()); // slice with L = 0
        assert!(classify_packet_byte(0x23).is_err());
    }

    /// The 23-bit type-1 I-slice header of spec/07 §11.
    fn worked_example_header() -> Packer {
        let mut p = Packer::new();
        p.ue(2); // slice_type I → `011`
        p.push(1, 0); // encrypted
        p.push(8, 0); // picture_id
        p.push(5, 13); // qp
        p.push(1, 0); // mb_qp_delta_enable
        p.push(1, 0); // flag
        p.push(1, 0); // mode
        p.push(2, 0); // reserved
        p.push(1, 0); // extension terminator
        p
    }

    #[test]
    fn parse_slice_header_worked_example_is_23_bits() {
        let p = worked_example_header();
        assert_eq!(p.len(), 23);
        let h = parse_slice_header(&p.into_bytes(), SliceVersion::V1, 2, 532, 300, false, false)
            .unwrap();
        assert_eq!(h.frame_type, Svq3FrameType::Intra);
        assert_eq!(h.encrypted, Some(false));
        assert_eq!(h.first_mb, None);
        assert_eq!(h.slice_qp, 13);
        assert!(!h.mb_qp_delta_enable);
        assert!(!h.mode);
        assert_eq!(h.extended, None);
        assert_eq!(h.header_end_bit, 23);
    }

    #[test]
    fn parse_slice_header_v2_first_mb_and_protected_bit() {
        let mut p = Packer::new();
        p.ue(0); // P
        p.push(9, 299); // first_mb, 300 MBs → 9 bits
        p.push(8, 7);
        p.push(5, 20);
        p.push(1, 1);
        p.push(1, 1);
        p.push(1, 1); // protected-only flag
        p.push(1, 0); // mode
        p.push(2, 0b10);
        p.push(1, 1);
        p.push(8, 0x5a);
        p.push(1, 0);
        let h =
            parse_slice_header(&p.into_bytes(), SliceVersion::V2, 1, 0, 300, true, false).unwrap();
        assert_eq!(h.frame_type, Svq3FrameType::Predicted);
        assert_eq!(h.first_mb, Some(299));
        assert_eq!(h.picture_id, 7);
        assert_eq!(h.slice_qp, 20);
        assert!(h.mb_qp_delta_enable);
        assert!(h.flag);
        assert_eq!(h.protected_flag, Some(true));
        assert_eq!(h.reserved, 0b10);
        assert_eq!(h.extension_bytes, vec![0x5a]);
    }

    #[test]
    fn parse_slice_header_mode_must_echo_seqh() {
        let p = worked_example_header();
        assert!(matches!(
            parse_slice_header(&p.into_bytes(), SliceVersion::V1, 1, 0, 4, false, true),
            Err(Error::InvalidFrameCode(0))
        ));
    }

    #[test]
    fn parse_slice_header_extended_mode_sequence() {
        // spec/07 §3.3 with c = 1 (d and e present), f = 1 (g present),
        // c ∉ {4, 5} (no h).
        let mut p = Packer::new();
        p.ue(2);
        p.push(1, 0);
        p.push(8, 3);
        p.push(5, 9);
        p.push(1, 0);
        p.push(1, 0);
        p.push(1, 1); // mode
        p.push(2, 1); // a
        p.push(2, 2); // b
        p.push(3, 1); // c
        p.push(3, 5); // d
        p.push(8, 0x77); // e
        p.push(2, 0); // reserved
        p.push(1, 0); // extension terminator
        p.push(1, 1); // f
        p.push(8, 0x42); // g
        p.ue(1);
        p.ue(2);
        p.ue(3);
        p.ue(4);
        p.push(1, 1);
        p.push(2, 0b11);
        let expected_len = p.len();
        let h =
            parse_slice_header(&p.into_bytes(), SliceVersion::V1, 1, 0, 4, false, true).unwrap();
        assert!(h.mode);
        let x = h.extended.unwrap();
        assert_eq!((x.a, x.b, x.c), (1, 2, 1));
        assert_eq!(x.d, Some(5));
        assert_eq!(x.e, Some(0x77));
        assert!(x.f);
        assert_eq!(x.g, Some(0x42));
        assert_eq!(x.h, None);
        assert_eq!(x.r, [1, 2, 3, 4]);
        assert!(x.tail1);
        assert_eq!(x.tail2, 0b11);
        assert_eq!(h.header_end_bit, expected_len);

        // c = 4: e and h present, d absent; a = 3 is an error.
        let mut p = Packer::new();
        p.ue(2);
        p.push(1, 0);
        p.push(8, 0);
        p.push(5, 0);
        p.push(1, 0);
        p.push(1, 0);
        p.push(1, 1);
        p.push(2, 0);
        p.push(2, 0);
        p.push(3, 4); // c
        p.push(8, 0x11); // e
        p.push(2, 0);
        p.push(1, 0);
        p.push(1, 0); // f
        p.push(3, 6); // h
        for _ in 0..4 {
            p.ue(0);
        }
        p.push(1, 0);
        p.push(2, 0);
        let h =
            parse_slice_header(&p.into_bytes(), SliceVersion::V1, 1, 0, 4, false, true).unwrap();
        let x = h.extended.unwrap();
        assert_eq!(x.d, None);
        assert_eq!(x.e, Some(0x11));
        assert_eq!(x.h, Some(6));
        assert_eq!(x.g, None);

        let mut p = Packer::new();
        p.ue(2);
        p.push(1, 0);
        p.push(8, 0);
        p.push(5, 0);
        p.push(1, 0);
        p.push(1, 0);
        p.push(1, 1);
        p.push(2, 3); // a = 3 → error
        p.push(8, 0);
        assert!(matches!(
            parse_slice_header(&p.into_bytes(), SliceVersion::V1, 1, 0, 4, false, true),
            Err(Error::InvalidFrameCode(3))
        ));
    }

    #[test]
    fn parse_slice_header_rejects_invalid_slice_type() {
        let mut p = Packer::new();
        p.ue(3);
        p.push(8, 0);
        assert_eq!(
            parse_slice_header(&p.into_bytes(), SliceVersion::V1, 1, 0, 4, false, false)
                .unwrap_err(),
            Error::InvalidFrameCode(3)
        );
    }

    /// Wrap an unpermuted payload in the on-wire envelope for `L`.
    fn wire(version: u8, length_width: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![(length_width << 5) | version];
        let len = payload.len() as u32;
        for i in (0..length_width).rev() {
            out.push((len >> (8 * i as u32)) as u8);
        }
        let moved = (length_width - 1) as usize;
        out.extend_from_slice(&payload[moved..]);
        out.extend_from_slice(&payload[..moved]);
        out
    }

    #[test]
    fn parse_wire_slice_round_trips_every_length_width() {
        let payload = worked_example_header().into_bytes();
        for l in 1..=3u8 {
            let mut w = wire(1, l, &payload);
            w.push(SVQ3_FRAME_END);
            let s = parse_wire_slice(&w, 300, false, false).unwrap();
            assert_eq!(s.header.slice_size_size, l);
            assert_eq!(s.header.slice_size, payload.len() as u32);
            assert_eq!(s.payload, payload);
            assert_eq!(s.consumed, w.len() - 1);
            assert_eq!(w[s.consumed], SVQ3_FRAME_END);
        }
    }

    #[test]
    fn parse_wire_slice_fixture_320x240_frame0_envelope() {
        // spec/07 §11: packet byte 0x41 → L = 2, type 1, length 532;
        // payload byte 0 is packet byte 534.
        let mut w = vec![0x41, 0x02, 0x14];
        let mut payload = worked_example_header().into_bytes();
        payload.resize(532, 0);
        payload[0] = 0x60;
        w.extend_from_slice(&payload[1..]);
        w.push(payload[0]);
        w.push(0xff);
        assert_eq!(w[534], 0x60);
        let s = parse_wire_slice(&w, 300, false, false).unwrap();
        assert_eq!(s.header.slice_size, 532);
        assert_eq!(s.payload[0], 0x60);
        assert_eq!(s.consumed, 535);
    }

    #[test]
    fn parse_wire_slice_rejections() {
        assert_eq!(
            parse_wire_slice(&[], 4, false, false).unwrap_err(),
            Error::Truncated
        );
        assert_eq!(
            parse_wire_slice(&[0xff], 4, false, false).unwrap_err(),
            Error::Truncated
        );
        assert!(parse_wire_slice(&[0x23, 0x01, 0x80], 4, false, false).is_err());
        assert_eq!(
            parse_wire_slice(&[0x01, 0x01, 0x80], 4, false, false).unwrap_err(),
            Error::BadBitWidth(0)
        );
        assert_eq!(
            parse_wire_slice(&[0x41, 0x00], 4, false, false).unwrap_err(),
            Error::Truncated
        );
        assert_eq!(
            parse_wire_slice(&[0x21, 0x09, 0x80, 0x00], 4, false, false).unwrap_err(),
            Error::Truncated
        );
    }
}
