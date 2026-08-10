//! SVQ3 sequence- and slice-header parser.
//!
//! Implements the bit-packed SVQ3 sequence header (`SEQH` extradata)
//! and the per-slice header described in
//! `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Sequence Header" /
//! §"Slice Header". The wiki page is a verbatim local mirror of the
//! multimedia.cx Sorenson_Video_3 wiki page (fetched 2026-05-06,
//! CC-BY-SA per multimedia.cx terms) and is the only authoritative
//! source consulted during round 3.
//!
//! Round 3 is **structural**: this module returns typed
//! [`Svq3SequenceHeader`] / [`Svq3SliceHeader`] values that capture
//! every wiki-spec'd field, without performing any motion-compensation
//! / residual / dequantisation work. The bitstream beyond the slice
//! header (macroblock layer, Golomb-coded coefficients, …) is **not**
//! decoded — those phases are tracked separately and are out of round-3
//! scope.
//!
//! ## What the parser does **not** do
//!
//! * It does not decode macroblock-layer Golomb / VLC payload.
//! * It does not interpret the protected-stream watermark sub-record
//!   (the wiki spec describes the byte layout but the deflated
//!   watermark image's role is purely as input to a separate
//!   decryption stage that is not yet wired up). The parser captures
//!   the structural slot via [`Svq3SequenceHeader::protected`] and
//!   leaves the variable-length watermark fields for a later round.
//! * It does not apply slice-payload permutation / de-scrambling. The
//!   wiki spec's §"Slice Header" describes the permutation as "the
//!   first 0-2 bytes (size of slice size minus one) … are stored at
//!   the very end of the slice"; this module exposes
//!   [`unpermute_slice_payload`] as a helper that reverses that
//!   reordering, but the further "may be further scrambled" descramble
//!   step is left to a later round once the descramble algorithm is
//!   pinned in `docs/`.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::header::FRAME_SIZE_TABLE;

/// SVQ3 sequence-header marker bytes (`"SEQH"`).
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Sequence Header"
/// the sequence header is stored in an `SMI ` atom whose payload
/// starts with the literal four-byte ASCII marker `"SEQH"`, followed
/// by a four-byte big-endian length, followed by the bit-packed
/// sequence-header payload of that declared length. The same marker
/// is referenced in §"Packetization" where it notes "SVQ3 decoders
/// expect extradata to be prefixed with the marker bytes 'SEQH',
/// followed by another 4 bytes indicating the length of the extradata".
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub const SVQ3_SEQH_MAGIC: [u8; 4] = *b"SEQH";

/// SVQ3 frame-data sentinel: a single `0xFF` byte at the position
/// where the next slice header would be indicates frame-data end (per
/// the wiki spec's §"Slice Header" opening sentence).
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub const SVQ3_FRAME_END: u8 = 0xFF;

/// Decoded SVQ3 sequence header (`SEQH` extradata payload).
///
/// All fields follow the wiki spec's §"Sequence Header" naming. The
/// trailing optional-data byte run is captured as
/// [`Self::optional_bytes`]; the protected-stream sub-record is
/// captured at a structural level via [`Self::protected`] only, with
/// the variable-length watermark fields deferred (see module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svq3SequenceHeader {
    /// Raw 3-bit frame-size code. `7` indicates the explicit-12+12-bit
    /// width/height escape was used; in that case [`Self::width`] and
    /// [`Self::height`] carry the explicit values rather than the
    /// [`FRAME_SIZE_TABLE`] lookup.
    pub frame_size_code: u8,
    /// Decoded width in pixels — either from the
    /// [`FRAME_SIZE_TABLE`] lookup (codes 0..=6) or from the explicit
    /// 12-bit width field (code 7).
    pub width: u16,
    /// Decoded height in pixels. See [`Self::width`].
    pub height: u16,
    /// `true` iff the stream is allowed to use halfpel-precision
    /// motion vectors.
    pub has_halfpel: bool,
    /// `true` iff the stream is allowed to use thirdpel-precision
    /// motion vectors.
    pub has_thirdpel: bool,
    /// Raw 4-bit "unknown" field from the wiki spec. Captured for
    /// future use; semantics are not yet documented in the spec.
    pub unknown4: u8,
    /// `true` iff the wiki spec's "stream does not contain B-frames"
    /// flag is set.
    pub no_b_frames: bool,
    /// Captured run of optional bytes from the wiki spec's
    /// `1 bit - has more data, 8 bits - data, repeat` loop. Empty
    /// when the loop's first peek-bit was 0.
    pub optional_bytes: Vec<u8>,
    /// `true` iff the wiki spec's "stream is protected" flag is set.
    /// When set, additional watermark fields follow that this round
    /// does **not** parse (the variable-length watermark sub-record
    /// is deferred — see module docs).
    pub protected: bool,
    /// Bit position (relative to the start of the sequence-header
    /// payload, i.e. just past the SEQH 4-byte marker + length prefix)
    /// at which the parser stopped reading. For a non-protected
    /// stream this is past the protected-flag bit; for a protected
    /// stream this points at the first bit of the variable-length
    /// watermark sub-record that round 3 does not consume.
    pub header_end_bit: usize,
}

/// SVQ3 slice-header version field.
///
/// Per the wiki spec only versions 1 and 2 are known; either picks a
/// distinct interpretation of the field that follows the variable-length
/// frame-code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceVersion {
    /// Version 1 — followed by a single "has more slices" 1-bit flag.
    V1,
    /// Version 2 — followed by a `max(ceil_log2(num_mbs), 6)`-bit
    /// macroblock-offset field.
    V2,
}

/// SVQ3 frame-code field (variable-length, values 0/1/2).
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Slice Header"
/// the first variable-length code in the slice payload disambiguates
/// the slice's enclosing frame type: `0 = P`, `1 = B`, `2 = I`. The
/// values 0/1/2 are carried in the universal code of
/// `docs/video/svq3/spec/06-residual-coefficient-coding.md` §1 as
/// `1`, `010`, `011` respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq3FrameType {
    /// Predicted frame (P) — Golomb code `0`.
    Predicted,
    /// Bidirectionally-predicted frame (B) — Golomb code `1`.
    Bidirectional,
    /// Intra frame (I) — Golomb code `2`.
    Intra,
}

impl Svq3FrameType {
    /// Map the variable-length frame-code value (0/1/2) to the typed
    /// enum.
    pub fn from_code(code: u32) -> Result<Self> {
        match code {
            0 => Ok(Self::Predicted),
            1 => Ok(Self::Bidirectional),
            2 => Ok(Self::Intra),
            other => Err(Error::InvalidFrameCode(other)),
        }
    }
}

/// SVQ3 slice header.
///
/// All fields follow the wiki spec's §"Slice Header" naming. The
/// trailing optional-data byte run is captured as
/// [`Self::optional_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svq3SliceHeader {
    /// Slice-header version from the low 5 bits of the slice's first
    /// byte. Either [`SliceVersion::V1`] or [`SliceVersion::V2`].
    pub version: SliceVersion,
    /// Slice-size-size value from the high 3 bits of the slice's first
    /// byte. The wiki spec restricts this to the range 1..=3 since
    /// the slice size itself is 1-3 bytes; any other value triggers a
    /// parse error.
    pub slice_size_size: u8,
    /// Declared payload size in bytes (read from the 1-3 byte big-
    /// endian slice-size field that follows the 1-byte version /
    /// size-size prefix).
    pub slice_size: u32,
    /// Decoded frame-code field (variable-length).
    pub frame_type: Svq3FrameType,
    /// Version-1-only "has more slices" 1-bit flag; `None` on
    /// [`SliceVersion::V2`].
    pub has_more_slices_v1: Option<bool>,
    /// Version-2-only macroblock-offset field. The bit-width is
    /// `max(ceil_log2(num_mbs), 6)` per the wiki spec; the parser is
    /// passed `num_mbs` by the caller (the sequence header's
    /// width/height combined with the 16-pixel macroblock grid yields
    /// `num_mbs`). `None` on [`SliceVersion::V1`].
    pub mb_offset_v2: Option<u32>,
    /// 8-bit frame number field.
    pub frame_number: u8,
    /// 5-bit slice-quantiser field.
    pub slice_qp: u8,
    /// `true` iff the wiki spec's "delta quantiser may be present"
    /// flag is set.
    pub delta_qp_present: bool,
    /// Raw unnamed 1-bit "unknown" flag from the wiki spec.
    pub unknown1: bool,
    /// Protected-stream-only extra 1-bit flag, present iff the parser
    /// was invoked with `protected = true`. `None` when the stream
    /// is not protected.
    pub protected_unknown: Option<bool>,
    /// Captured run of optional bytes from the wiki spec's
    /// `1 bit - has more data, 8 bits - data, repeat` loop. Empty
    /// when the loop's first peek-bit was 0.
    pub optional_bytes: Vec<u8>,
    /// Bit position (relative to the start of the unpermuted slice
    /// payload) at which the parser stopped reading. The macroblock-
    /// layer Golomb stream begins at this offset.
    pub header_end_bit: usize,
}

impl Svq3SliceHeader {
    /// Convenience predicate — `true` iff the slice belongs to an
    /// I-frame.
    pub fn is_intra(&self) -> bool {
        self.frame_type == Svq3FrameType::Intra
    }
}

/// Number of 16×16 macroblocks the sequence-header width/height
/// implies, rounding up at the right/bottom edges. The wiki spec's
/// §"Slice Header" v2 macroblock-offset field is `max(ceil_log2(num_mbs),
/// 6)` bits wide where `num_mbs` is the total picture macroblock count;
/// this helper derives that count from a parsed
/// [`Svq3SequenceHeader`].
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn num_macroblocks(seqh: &Svq3SequenceHeader) -> u32 {
    let (mb_w, mb_h) = mb_grid_dims(seqh);
    mb_w * mb_h
}

/// The macroblock grid dimensions `(mb_cols, mb_rows)` implied by the
/// sequence-header width/height, rounding up at the right/bottom edges
/// (the standard 16×16-macroblock tiling: a partial edge macroblock
/// still occupies a full grid cell).
///
/// `mb_cols * mb_rows == num_macroblocks(seqh)`.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn mb_grid_dims(seqh: &Svq3SequenceHeader) -> (u32, u32) {
    let mb_w = (seqh.width as u32).div_ceil(16);
    let mb_h = (seqh.height as u32).div_ceil(16);
    (mb_w, mb_h)
}

/// Raster position of one macroblock within the picture's macroblock
/// grid, plus the intra-prediction neighbour availability the
/// per-macroblock intra-mode decode + reconstruction needs.
///
/// Produced by [`macroblock_position`] from a macroblock raster index
/// and the grid dimensions. The neighbour-availability flags say whether
/// a macroblock exists immediately above / to the left in the **same
/// picture** — `mb_y > 0` / `mb_x > 0` respectively. (SVQ3 intra
/// prediction reads only the above and left neighbours; per
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra macroblock
/// information decoding" an out-of-slice predictor is treated as `-1`.)
/// Slice boundaries may further restrict availability — that is the
/// caller's concern once the slice-level walk threads slice membership;
/// this helper provides the picture-grid geometry.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Svq3MacroblockPosition {
    /// Macroblock column (`0..mb_cols`).
    pub mb_x: u32,
    /// Macroblock row (`0..mb_rows`).
    pub mb_y: u32,
    /// `true` iff a macroblock exists immediately above (`mb_y > 0`).
    pub top_available: bool,
    /// `true` iff a macroblock exists immediately to the left
    /// (`mb_x > 0`).
    pub left_available: bool,
}

impl Svq3MacroblockPosition {
    /// Pixel origin `(x, y)` of this macroblock's top-left luma sample
    /// (`mb_x * 16`, `mb_y * 16`).
    pub fn luma_origin(&self) -> (u32, u32) {
        (self.mb_x * 16, self.mb_y * 16)
    }
}

/// Compute the [`Svq3MacroblockPosition`] of the macroblock at raster
/// index `mb_index` (`0..mb_cols * mb_rows`) in a `mb_cols`-wide
/// macroblock grid.
///
/// Macroblocks are decoded in raster order (left-to-right, top-to-
/// bottom). Returns [`Error::BadBitWidth`] (re-used as a generic
/// argument-domain error) when `mb_cols == 0`; the caller is
/// responsible for keeping `mb_index < mb_cols * mb_rows`.
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

/// `ceil(log2(value))` for `value >= 1`. Used by [`parse_slice_header`]
/// to compute the v2 macroblock-offset field width.
fn ceil_log2(value: u32) -> u32 {
    if value <= 1 {
        return 0;
    }
    32 - (value - 1).leading_zeros()
}

/// Strip the `"SEQH"` marker + 4-byte big-endian length prefix from
/// the SVQ3 extradata.
///
/// Per the wiki spec's §"Sequence Header" + §"Packetization", SVQ3
/// extradata begins with the literal four-byte `"SEQH"` marker
/// followed by a four-byte big-endian length, followed by the bit-
/// packed payload of that declared length. This helper validates the
/// marker, reads the length, bounds-checks against the input slice,
/// and returns the payload slice for [`parse_sequence_header`].
///
/// Returns [`Error::InvalidFrameCode`] (re-used for SEQH-magic
/// mismatches since the magic is itself a structural framing
/// invariant) when the magic does not match, and [`Error::Truncated`]
/// when the length prefix or declared payload runs off the end of the
/// input.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn strip_seqh_prefix(extradata: &[u8]) -> Result<&[u8]> {
    if extradata.len() < 8 {
        return Err(Error::Truncated);
    }
    if extradata[..4] != SVQ3_SEQH_MAGIC {
        // The SEQH magic is a structural invariant; treat a mismatch
        // as an invalid frame code so callers see a single structural-
        // error variant.
        let bad = u32::from_be_bytes([extradata[0], extradata[1], extradata[2], extradata[3]]);
        return Err(Error::InvalidFrameCode(bad));
    }
    let declared_len =
        u32::from_be_bytes([extradata[4], extradata[5], extradata[6], extradata[7]]) as usize;
    let payload = &extradata[8..];
    if payload.len() < declared_len {
        return Err(Error::Truncated);
    }
    Ok(&payload[..declared_len])
}

/// Parse the bit-packed SVQ3 sequence-header payload (the bytes
/// **past** the `"SEQH"` marker + 4-byte length prefix).
///
/// See [`strip_seqh_prefix`] for a helper that handles the prefix.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn parse_sequence_header(payload: &[u8]) -> Result<Svq3SequenceHeader> {
    let mut br = BitReader::new(payload);

    // 3-bit frame size code.
    let frame_size_code = br.read_bits(3)? as u8;
    let (width, height) = match frame_size_code {
        7 => {
            let w = br.read_bits(12)? as u16;
            let h = br.read_bits(12)? as u16;
            (w, h)
        }
        // 3-bit read guarantees the value is in 0..=7; the `7` arm
        // above peels off the explicit-dimensions case, so `other` is
        // guaranteed in 0..=6 here, which is a valid index into the
        // 7-entry `FRAME_SIZE_TABLE` lookup.
        other => FRAME_SIZE_TABLE[other as usize],
    };

    // 1-bit halfpel flag, 1-bit thirdpel flag.
    let has_halfpel = br.read_bit()? == 1;
    let has_thirdpel = br.read_bit()? == 1;

    // 4-bit "unknown" field.
    let unknown4 = br.read_bits(4)? as u8;

    // 1-bit "no B-frames" flag.
    let no_b_frames = br.read_bit()? == 1;

    // Optional-data byte run: while peek-bit==1, read a byte.
    let mut optional_bytes = Vec::new();
    loop {
        let flag = br.read_bit()?;
        if flag == 0 {
            break;
        }
        optional_bytes.push(br.read_bits(8)? as u8);
    }

    // 1-bit "stream is protected" flag.
    let protected = br.read_bit()? == 1;

    // The protected-stream sub-record (watermark width / height /
    // unknowns + deflated watermark image) is described by the wiki
    // spec but its variable-length-code encoding and watermark-image
    // decryption are out of round-3 scope. The parser captures the
    // structural slot via `protected` and stops here.

    Ok(Svq3SequenceHeader {
        frame_size_code,
        width,
        height,
        has_halfpel,
        has_thirdpel,
        unknown4,
        no_b_frames,
        optional_bytes,
        protected,
        header_end_bit: br.bits_consumed(),
    })
}

/// Convenience: combine [`strip_seqh_prefix`] + [`parse_sequence_header`]
/// for callers that hold the raw extradata blob.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn parse_extradata(extradata: &[u8]) -> Result<Svq3SequenceHeader> {
    let payload = strip_seqh_prefix(extradata)?;
    parse_sequence_header(payload)
}

/// Reverse the slice-payload permutation described by
/// `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Slice Header".
///
/// The wiki spec describes the on-wire layout as:
///
/// > "Slice data is stored in permuted form: bits 5-7 of the first
/// > byte tell the size of the slice size in bytes, then you have 1-3
/// > bytes for slice size, then you have most of the payload except
/// > for the first 0-2 bytes (size of slice size minus one) which are
/// > stored at the very end of the slice."
///
/// So if `slice_size_size` is `S` bytes (range 1..=3), the first
/// `S - 1` bytes of the payload were moved to the slice trailer.
/// This helper rebuilds the unpermuted payload by taking the trailing
/// `S - 1` bytes and prepending them. For `S == 1` (no permutation)
/// the function returns the input unchanged.
///
/// The `body` argument is the slice payload **excluding** the 1-byte
/// version/size-size prefix and the 1-3 byte slice-size field — i.e.
/// the `slice_size` bytes that immediately follow the header.
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

/// Decode one codeword of SVQ3's single universal variable-length
/// code, returning its non-negative **code number**.
///
/// Per `docs/video/svq3/spec/06-residual-coefficient-coding.md` §1,
/// SVQ3 carries every variable-length element — macroblock type,
/// coded-block pattern, quantiser delta, motion-vector components, and
/// every residual symbol — as a code number in one and the same code.
/// The code is read MSB-first and is `2·n + 1` bits long; it carries
/// `n` data bits and
///
/// ```text
/// code_number = 2ⁿ − 1 + value      (value = the n data bits, MSB first)
/// ```
///
/// The bit layout interleaves terminator bits among the data bits (so
/// a decoder never looks further ahead than the next bit):
///
/// 1. one bit; `1` ⇒ `n = 0`, code number 0, done.
/// 2. one more bit; `1` ⇒ `n = 1`: one data bit follows (3 bits total).
/// 3. otherwise (`n ≥ 2`) two data bits, then a terminator bit; a `1`
///    terminator ends the code, a `0` extends it by one more data bit
///    and one more terminator, raising `n` by one each time.
///
/// For `n ≤ 1` this coincides bit-for-bit with the familiar
/// exp-Golomb layout (so the 0/1/2 alphabet of the slice frame-code
/// field is unchanged); from `n = 2` upward the two layouts differ,
/// which is why this reader — not a standard exp-Golomb reader — must
/// be used for every macroblock-layer element.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn read_universal_code(br: &mut BitReader<'_>) -> Result<u32> {
    if br.read_bit()? == 1 {
        return Ok(0);
    }
    if br.read_bit()? == 1 {
        let d = br.read_bit()? as u32;
        return Ok(1 + d);
    }
    // n >= 2: two data bits, then terminator-interleaved extensions.
    let mut value = ((br.read_bit()? as u32) << 1) | br.read_bit()? as u32;
    let mut n: u32 = 2;
    while br.read_bit()? == 0 {
        n += 1;
        if n > 31 {
            // A 32-data-bit code cannot be represented in the u32 code
            // number space; treat it as a malformed stream.
            return Err(Error::BadBitWidth(n));
        }
        value = (value << 1) | br.read_bit()? as u32;
    }
    Ok((1u32 << n) - 1 + value)
}

/// Parse a single SVQ3 slice header from the start of `unpermuted_body`.
///
/// The caller is responsible for (a) reading the 1-byte version/
/// size-size prefix + 1-3 byte slice-size field from the wire to
/// learn how many payload bytes constitute the slice body, (b)
/// running [`unpermute_slice_payload`] over that body, and (c)
/// invoking this function with the unpermuted body plus the
/// already-decoded `version`, `slice_size_size`, and `slice_size`
/// values. `num_mbs` is the picture macroblock count (see
/// [`num_macroblocks`]) and `protected` mirrors the sequence header's
/// `protected` flag.
///
/// Returns the populated [`Svq3SliceHeader`].
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn parse_slice_header(
    unpermuted_body: &[u8],
    version: SliceVersion,
    slice_size_size: u8,
    slice_size: u32,
    num_mbs: u32,
    protected: bool,
) -> Result<Svq3SliceHeader> {
    let mut br = BitReader::new(unpermuted_body);

    // Variable-length frame-code field (universal code; 0=P, 1=B, 2=I).
    let frame_code = read_universal_code(&mut br)?;
    let frame_type = Svq3FrameType::from_code(frame_code)?;

    // Version-dependent field after frame code.
    let mut has_more_slices_v1 = None;
    let mut mb_offset_v2 = None;
    match version {
        SliceVersion::V1 => {
            has_more_slices_v1 = Some(br.read_bit()? == 1);
        }
        SliceVersion::V2 => {
            let width = ceil_log2(num_mbs).max(6);
            mb_offset_v2 = Some(br.read_bits(width)?);
        }
    }

    // 8-bit frame number.
    let frame_number = br.read_bits(8)? as u8;
    // 5-bit slice quantiser.
    let slice_qp = br.read_bits(5)? as u8;
    // 1-bit "delta quantiser may be present" flag.
    let delta_qp_present = br.read_bit()? == 1;
    // 1-bit "unknown" flag.
    let unknown1 = br.read_bit()? == 1;
    // Protected-only extra 1-bit "unknown" flag.
    let protected_unknown = if protected {
        Some(br.read_bit()? == 1)
    } else {
        None
    };

    // Optional-data byte run: while peek-bit==1, read a byte.
    let mut optional_bytes = Vec::new();
    loop {
        let flag = br.read_bit()?;
        if flag == 0 {
            break;
        }
        optional_bytes.push(br.read_bits(8)? as u8);
    }

    Ok(Svq3SliceHeader {
        version,
        slice_size_size,
        slice_size,
        frame_type,
        has_more_slices_v1,
        mb_offset_v2,
        frame_number,
        slice_qp,
        delta_qp_present,
        unknown1,
        protected_unknown,
        optional_bytes,
        header_end_bit: br.bits_consumed(),
    })
}

/// Convenience: read the slice's 1-byte prefix + 1-3 byte slice-size
/// field + unpermute the body + parse the slice header in one call.
///
/// `wire_slice` is the on-wire slice bytes starting at the 1-byte
/// version/size-size prefix and including the full slice body. The
/// returned tuple is `(header, body_after_header_bytes)`, where the
/// second element is the slice-body bytes past the parsed header
/// (i.e. macroblock-layer Golomb stream input for a later round).
///
/// A leading `0xFF` byte aborts with [`Error::Truncated`] and is the
/// caller's signal to stop walking slices (per the wiki spec's
/// "`0xFF` means frame data end" rule). For typed end-of-frame
/// signalling callers should check `wire_slice[0] == SVQ3_FRAME_END`
/// before invoking this function.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn parse_wire_slice(
    wire_slice: &[u8],
    num_mbs: u32,
    protected: bool,
) -> Result<(Svq3SliceHeader, Vec<u8>)> {
    if wire_slice.is_empty() {
        return Err(Error::Truncated);
    }
    if wire_slice[0] == SVQ3_FRAME_END {
        // Frame-end sentinel — caller should detect this before
        // invocation, but reject defensively rather than parse
        // garbage.
        return Err(Error::Truncated);
    }

    let prefix = wire_slice[0];
    let slice_size_size = (prefix >> 5) & 0x07;
    let version_bits = prefix & 0x1F;
    let version = match version_bits {
        1 => SliceVersion::V1,
        2 => SliceVersion::V2,
        // Per the wiki spec "only versions 1 and 2 are known"; any
        // other value is treated as an invalid frame-code-style
        // structural failure.
        other => return Err(Error::InvalidFrameCode(other as u32)),
    };
    if !(1..=3).contains(&slice_size_size) {
        return Err(Error::BadBitWidth(slice_size_size as u32));
    }
    let want = 1 + slice_size_size as usize;
    if wire_slice.len() < want {
        return Err(Error::Truncated);
    }
    let mut slice_size: u32 = 0;
    for i in 0..(slice_size_size as usize) {
        slice_size = (slice_size << 8) | wire_slice[1 + i] as u32;
    }
    let body_start = want;
    let body_end = body_start
        .checked_add(slice_size as usize)
        .ok_or(Error::Truncated)?;
    if wire_slice.len() < body_end {
        return Err(Error::Truncated);
    }
    let body = &wire_slice[body_start..body_end];
    let unpermuted = unpermute_slice_payload(body, slice_size_size)?;
    let header = parse_slice_header(
        &unpermuted,
        version,
        slice_size_size,
        slice_size,
        num_mbs,
        protected,
    )?;
    // The bytes past the slice header (macroblock-layer Golomb stream
    // input). Round 3 does not consume them; surface them for a
    // future round.
    let consumed_bits = header.header_end_bit;
    let consumed_bytes = consumed_bits / 8;
    let remainder = if consumed_bytes >= unpermuted.len() {
        Vec::new()
    } else {
        unpermuted[consumed_bytes..].to_vec()
    };
    Ok((header, remainder))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: pack a sequence of `(width, value)` items into a byte
    /// stream by writing them MSB-first. Mirrors the helper used by
    /// `header::tests` so SVQ3 tests can build fixtures without
    /// re-implementing the bit-packing.
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

    /// Encode one universal-code codeword (spec/06 §1 interleaved
    /// layout) as a `(width, value)` pack item.
    fn uvlc(code: u32) -> (u32, u32) {
        // n = floor(log2(code + 1)); value = code + 1 - 2^n.
        let n = 31 - (code + 1).leading_zeros();
        let value = code + 1 - (1u32 << n);
        match n {
            0 => (1, 1),
            1 => (3, 0b010 | value),
            _ => {
                // 0 0 d1 d2 [0 d]* 1 — data bits MSB first.
                let mut bits: u64 = 0;
                let mut len: u32 = 0;
                let push = |b: u32, bits: &mut u64, len: &mut u32| {
                    *bits = (*bits << 1) | b as u64;
                    *len += 1;
                };
                push(0, &mut bits, &mut len);
                push(0, &mut bits, &mut len);
                push((value >> (n - 1)) & 1, &mut bits, &mut len);
                push((value >> (n - 2)) & 1, &mut bits, &mut len);
                for i in (0..n - 2).rev() {
                    push(0, &mut bits, &mut len);
                    push((value >> i) & 1, &mut bits, &mut len);
                }
                push(1, &mut bits, &mut len);
                (len, bits as u32)
            }
        }
    }

    /// Wrap a payload bit-string in the `"SEQH"` 4-byte marker + 4-byte
    /// big-endian length prefix.
    fn wrap_seqh(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&SVQ3_SEQH_MAGIC);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn ceil_log2_known_values() {
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(7), 3);
        assert_eq!(ceil_log2(8), 3);
        assert_eq!(ceil_log2(9), 4);
        assert_eq!(ceil_log2(16), 4);
        assert_eq!(ceil_log2(17), 5);
    }

    #[test]
    fn universal_code_round_trip() {
        // spec/06 §1 interleaved layout:
        //   0 = "1", 1 = "010", 2 = "011" (identical to the n ≤ 1
        //   exp-Golomb layout), then n = 2: "00 d1 d2 1" so
        //   3 = "00001", 4 = "00011", 5 = "00101", 6 = "00111",
        //   and n = 3: "00 d1 d2 0 d3 1" so 7 = "0000001",
        //   8 = "0000011", 10 = "0001011", 14 = "0011011".
        let cases: &[(&[u8], u32)] = &[
            (&[0b1000_0000], 0),
            (&[0b0100_0000], 1),
            (&[0b0110_0000], 2),
            (&[0b0000_1000], 3),
            (&[0b0001_1000], 4),
            (&[0b0010_1000], 5),
            (&[0b0011_1000], 6),
            (&[0b0000_0010], 7),
            (&[0b0000_0110], 8),
            (&[0b0001_0110], 10),
            (&[0b0011_0110], 14),
        ];
        for &(bytes, expected) in cases {
            let mut br = BitReader::new(bytes);
            assert_eq!(read_universal_code(&mut br).unwrap(), expected, "{bytes:?}");
        }
    }

    /// Cross-check the reader against the closed form for every code
    /// number the `uvlc` test encoder can produce in `0..=2000`.
    #[test]
    fn universal_code_encoder_decoder_agree() {
        for code in 0..=2000u32 {
            let bytes = pack(&[uvlc(code)]);
            let mut br = BitReader::new(&bytes);
            assert_eq!(read_universal_code(&mut br).unwrap(), code, "code {code}");
            // The code is 2n+1 bits: reconstruct n from the closed form.
            let n = 31 - (code + 1).leading_zeros();
            assert_eq!(br.bits_consumed() as u32, 2 * n + 1, "code {code} length");
        }
    }

    #[test]
    fn universal_code_truncated_input_errors() {
        // All-zero input → the terminator-extension loop runs out of
        // bits before a `1` terminator arrives.
        let mut br = BitReader::new(&[0x00]);
        assert!(matches!(
            read_universal_code(&mut br),
            Err(Error::Truncated)
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
        let err = Svq3FrameType::from_code(7).unwrap_err();
        assert!(matches!(err, Error::InvalidFrameCode(7)));
    }

    #[test]
    fn strip_seqh_prefix_rejects_short_input() {
        assert!(matches!(strip_seqh_prefix(b""), Err(Error::Truncated)));
        assert!(matches!(strip_seqh_prefix(b"SEQH"), Err(Error::Truncated)));
        assert!(matches!(
            strip_seqh_prefix(b"SEQH\x00\x00"),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn strip_seqh_prefix_rejects_bad_magic() {
        // Magic mismatch should surface as InvalidFrameCode (we reuse
        // the variant for structural-framing failures).
        let bad = b"BOGUS\x00\x00\x00\x00";
        assert!(matches!(
            strip_seqh_prefix(bad),
            Err(Error::InvalidFrameCode(_))
        ));
    }

    #[test]
    fn strip_seqh_prefix_returns_declared_payload() {
        let payload = [0xAA, 0xBB, 0xCC, 0xDD];
        let wrapped = wrap_seqh(&payload);
        let stripped = strip_seqh_prefix(&wrapped).unwrap();
        assert_eq!(stripped, payload.as_slice());
    }

    #[test]
    fn strip_seqh_prefix_truncated_when_declared_overflows() {
        // Magic + length declaring 10 bytes but only 2 follow.
        let mut blob = Vec::from(&SVQ3_SEQH_MAGIC[..]);
        blob.extend_from_slice(&10u32.to_be_bytes());
        blob.push(0xAA);
        blob.push(0xBB);
        assert!(matches!(strip_seqh_prefix(&blob), Err(Error::Truncated)));
    }

    /// Minimal SEQH: frame size code 2 (176×144), all flags off, no
    /// optional bytes, not protected. 3+1+1+4+1+1+1 = 12 bits.
    #[test]
    fn parse_seqh_minimal() {
        // [3 fsc=2] [1 halfpel=0] [1 thirdpel=0] [4 unknown=0]
        // [1 no_b=0] [1 optional_flag=0] [1 protected=0]
        let payload = pack(&[(3, 2), (1, 0), (1, 0), (4, 0), (1, 0), (1, 0), (1, 0)]);
        let h = parse_sequence_header(&payload).expect("valid SEQH");
        assert_eq!(h.frame_size_code, 2);
        assert_eq!(h.width, 176);
        assert_eq!(h.height, 144);
        assert!(!h.has_halfpel);
        assert!(!h.has_thirdpel);
        assert_eq!(h.unknown4, 0);
        assert!(!h.no_b_frames);
        assert!(h.optional_bytes.is_empty());
        assert!(!h.protected);
        assert_eq!(h.header_end_bit, 12);
    }

    /// SEQH with frame-size-code 7: explicit 12+12 dimensions.
    #[test]
    fn parse_seqh_size_code_7_explicit_dimensions() {
        let payload = pack(&[
            (3, 7),
            (12, 800),
            (12, 600),
            (1, 1),
            (1, 1),
            (4, 0xA),
            (1, 1),
            (1, 0),
            (1, 0),
        ]);
        let h = parse_sequence_header(&payload).expect("valid SEQH");
        assert_eq!(h.frame_size_code, 7);
        assert_eq!(h.width, 800);
        assert_eq!(h.height, 600);
        assert!(h.has_halfpel);
        assert!(h.has_thirdpel);
        assert_eq!(h.unknown4, 0xA);
        assert!(h.no_b_frames);
        assert!(h.optional_bytes.is_empty());
        assert!(!h.protected);
    }

    /// SEQH with the optional-byte trailer firing twice then stopping.
    #[test]
    fn parse_seqh_optional_byte_loop() {
        // [3 fsc=0] [1 halfpel=0] [1 thirdpel=0] [4 unknown=0]
        // [1 no_b=0]
        // optional loop: [1 1] [8 0x5A] [1 1] [8 0xA5] [1 0]
        // [1 protected=1]
        let payload = pack(&[
            (3, 0),
            (1, 0),
            (1, 0),
            (4, 0),
            (1, 0),
            (1, 1),
            (8, 0x5A),
            (1, 1),
            (8, 0xA5),
            (1, 0),
            (1, 1),
        ]);
        let h = parse_sequence_header(&payload).expect("valid SEQH");
        assert_eq!(h.optional_bytes, vec![0x5A, 0xA5]);
        assert!(h.protected);
    }

    /// SEQH frame-size-code lookup-table exhaustive — codes 0..=6.
    #[test]
    fn parse_seqh_frame_size_table() {
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
            let payload = pack(&[
                (3, code as u32),
                (1, 0),
                (1, 0),
                (4, 0),
                (1, 0),
                (1, 0),
                (1, 0),
            ]);
            let hdr = parse_sequence_header(&payload).expect("valid SEQH");
            assert_eq!(hdr.width, w, "code {code} width");
            assert_eq!(hdr.height, h, "code {code} height");
        }
    }

    #[test]
    fn parse_extradata_wraps_and_parses() {
        let payload = pack(&[(3, 2), (1, 0), (1, 0), (4, 0), (1, 0), (1, 0), (1, 0)]);
        let wrapped = wrap_seqh(&payload);
        let h = parse_extradata(&wrapped).expect("valid extradata");
        assert_eq!(h.width, 176);
        assert_eq!(h.height, 144);
    }

    #[test]
    fn num_macroblocks_rounds_up_edge() {
        // 176×144 → 11×9 = 99 MBs.
        let h = parse_sequence_header(&pack(&[
            (3, 2),
            (1, 0),
            (1, 0),
            (4, 0),
            (1, 0),
            (1, 0),
            (1, 0),
        ]))
        .unwrap();
        assert_eq!(num_macroblocks(&h), 99);

        // 800×600 → ceil(800/16)*ceil(600/16) = 50*38 = 1900.
        let h2 = parse_sequence_header(&pack(&[
            (3, 7),
            (12, 800),
            (12, 600),
            (1, 0),
            (1, 0),
            (4, 0),
            (1, 0),
            (1, 0),
            (1, 0),
        ]))
        .unwrap();
        assert_eq!(num_macroblocks(&h2), 1900);
    }

    #[test]
    fn mb_grid_dims_matches_num_macroblocks() {
        // 176×144 → 11×9.
        let h = parse_sequence_header(&pack(&[
            (3, 2),
            (1, 0),
            (1, 0),
            (4, 0),
            (1, 0),
            (1, 0),
            (1, 0),
        ]))
        .unwrap();
        let (cols, rows) = mb_grid_dims(&h);
        assert_eq!((cols, rows), (11, 9));
        assert_eq!(cols * rows, num_macroblocks(&h));
    }

    #[test]
    fn macroblock_position_raster_and_availability() {
        // 4-wide grid. Index 0 = top-left corner (no top, no left).
        let p0 = macroblock_position(0, 4).unwrap();
        assert_eq!((p0.mb_x, p0.mb_y), (0, 0));
        assert!(!p0.top_available && !p0.left_available);
        assert_eq!(p0.luma_origin(), (0, 0));

        // Index 1 = top row, col 1: left available, no top.
        let p1 = macroblock_position(1, 4).unwrap();
        assert_eq!((p1.mb_x, p1.mb_y), (1, 0));
        assert!(!p1.top_available && p1.left_available);

        // Index 4 = row 1, col 0: top available, no left.
        let p4 = macroblock_position(4, 4).unwrap();
        assert_eq!((p4.mb_x, p4.mb_y), (0, 1));
        assert!(p4.top_available && !p4.left_available);
        assert_eq!(p4.luma_origin(), (0, 16));

        // Index 5 = row 1, col 1: both available.
        let p5 = macroblock_position(5, 4).unwrap();
        assert_eq!((p5.mb_x, p5.mb_y), (1, 1));
        assert!(p5.top_available && p5.left_available);
        assert_eq!(p5.luma_origin(), (16, 16));
    }

    #[test]
    fn macroblock_position_rejects_zero_cols() {
        assert!(matches!(
            macroblock_position(0, 0),
            Err(Error::BadBitWidth(0))
        ));
    }

    /// Slice-payload permutation: `slice_size_size = 1` ⇒ no
    /// permutation (the body is returned unchanged).
    #[test]
    fn unpermute_slice_size_1_is_identity() {
        let body = [0x11, 0x22, 0x33, 0x44, 0x55];
        let out = unpermute_slice_payload(&body, 1).unwrap();
        assert_eq!(out, body);
    }

    /// Slice-payload permutation: `slice_size_size = 2` ⇒ the last
    /// 1 byte is prepended.
    #[test]
    fn unpermute_slice_size_2_moves_one_byte() {
        let body = [0x11, 0x22, 0x33, 0x44, 0xAA];
        let out = unpermute_slice_payload(&body, 2).unwrap();
        assert_eq!(out, vec![0xAA, 0x11, 0x22, 0x33, 0x44]);
    }

    /// Slice-payload permutation: `slice_size_size = 3` ⇒ the last
    /// 2 bytes are prepended in order.
    #[test]
    fn unpermute_slice_size_3_moves_two_bytes() {
        let body = [0x11, 0x22, 0x33, 0xAA, 0xBB];
        let out = unpermute_slice_payload(&body, 3).unwrap();
        assert_eq!(out, vec![0xAA, 0xBB, 0x11, 0x22, 0x33]);
    }

    #[test]
    fn unpermute_rejects_out_of_range_size() {
        let body = [0x11];
        assert!(matches!(
            unpermute_slice_payload(&body, 0),
            Err(Error::BadBitWidth(0))
        ));
        assert!(matches!(
            unpermute_slice_payload(&body, 4),
            Err(Error::BadBitWidth(4))
        ));
    }

    #[test]
    fn unpermute_rejects_truncated_body() {
        // slice_size_size = 3 needs at least 2 bytes; supply 1.
        let body = [0x11];
        assert!(matches!(
            unpermute_slice_payload(&body, 3),
            Err(Error::Truncated)
        ));
    }

    /// Slice-header parse, version 1, I-frame, num_mbs=99 (176×144).
    #[test]
    fn parse_slice_header_v1_iframe() {
        // ue(2) = "011" (frame code 2 = I).
        // v1 "has more slices" bit = 0.
        // 8 bits frame number = 42.
        // 5 bits quantiser = 17.
        // 1 bit delta-qp flag = 0.
        // 1 bit unknown = 0.
        // optional loop: 0 (stop).
        let unpermuted = pack(&[(3, 0b011), (1, 0), (8, 42), (5, 17), (1, 0), (1, 0), (1, 0)]);
        let hdr = parse_slice_header(&unpermuted, SliceVersion::V1, 1, 0, 99, false).unwrap();
        assert_eq!(hdr.frame_type, Svq3FrameType::Intra);
        assert!(hdr.is_intra());
        assert_eq!(hdr.has_more_slices_v1, Some(false));
        assert_eq!(hdr.mb_offset_v2, None);
        assert_eq!(hdr.frame_number, 42);
        assert_eq!(hdr.slice_qp, 17);
        assert!(!hdr.delta_qp_present);
        assert!(!hdr.unknown1);
        assert_eq!(hdr.protected_unknown, None);
        assert!(hdr.optional_bytes.is_empty());
    }

    /// Slice-header parse, version 2, P-frame, num_mbs=99 (so the
    /// macroblock-offset field is `max(ceil_log2(99), 6) = 7` bits).
    #[test]
    fn parse_slice_header_v2_pframe_mb_offset_7_bits() {
        // ue(0) = "1" (frame code 0 = P).
        // 7-bit MB offset = 100 (decimal).
        // 8 bits frame number = 5.
        // 5 bits qp = 10.
        // 1 bit delta_qp = 1.
        // 1 bit unknown = 0.
        // optional loop: 0.
        let unpermuted = pack(&[(1, 0b1), (7, 100), (8, 5), (5, 10), (1, 1), (1, 0), (1, 0)]);
        let hdr = parse_slice_header(&unpermuted, SliceVersion::V2, 1, 0, 99, false).unwrap();
        assert_eq!(hdr.frame_type, Svq3FrameType::Predicted);
        assert_eq!(hdr.has_more_slices_v1, None);
        assert_eq!(hdr.mb_offset_v2, Some(100));
        assert_eq!(hdr.frame_number, 5);
        assert_eq!(hdr.slice_qp, 10);
        assert!(hdr.delta_qp_present);
    }

    /// Slice-header parse with a B-frame and a protected stream — the
    /// extra protected-only 1-bit field is captured.
    #[test]
    fn parse_slice_header_protected_b_frame() {
        // ue(1) = "010" (B).
        // v1 "has more slices" = 1.
        // 8 bits frame_number = 3.
        // 5 bits qp = 20.
        // 1 bit delta_qp = 0.
        // 1 bit unknown = 1.
        // 1 bit protected_unknown = 1.
        // optional loop: 0.
        let unpermuted = pack(&[
            (3, 0b010),
            (1, 1),
            (8, 3),
            (5, 20),
            (1, 0),
            (1, 1),
            (1, 1),
            (1, 0),
        ]);
        let hdr = parse_slice_header(&unpermuted, SliceVersion::V1, 1, 0, 99, true).unwrap();
        assert_eq!(hdr.frame_type, Svq3FrameType::Bidirectional);
        assert!(hdr.unknown1);
        assert_eq!(hdr.protected_unknown, Some(true));
        assert_eq!(hdr.has_more_slices_v1, Some(true));
    }

    /// Slice-header parse with an optional-byte trailer that captures
    /// two bytes.
    #[test]
    fn parse_slice_header_optional_byte_loop() {
        // I-frame, v1, no more slices.
        // optional loop: 1 0x5A 1 0xA5 0.
        let unpermuted = pack(&[
            (3, 0b011),
            (1, 0),
            (8, 0),
            (5, 0),
            (1, 0),
            (1, 0),
            (1, 1),
            (8, 0x5A),
            (1, 1),
            (8, 0xA5),
            (1, 0),
        ]);
        let hdr = parse_slice_header(&unpermuted, SliceVersion::V1, 1, 0, 99, false).unwrap();
        assert_eq!(hdr.optional_bytes, vec![0x5A, 0xA5]);
    }

    /// Slice-header rejects a frame-code codeword that decodes to a
    /// value outside 0..=2.
    #[test]
    fn parse_slice_header_rejects_invalid_frame_code() {
        // Universal code 3 = "00001" → frame code 3, outside 0..=2.
        let unpermuted = pack(&[uvlc(3), (1, 0), (8, 0), (5, 0), (1, 0), (1, 0), (1, 0)]);
        let err = parse_slice_header(&unpermuted, SliceVersion::V1, 1, 0, 99, false).unwrap_err();
        assert!(matches!(err, Error::InvalidFrameCode(3)));
    }

    /// `parse_wire_slice` end-to-end: build a slice with prefix +
    /// size + permuted body, confirm the parser unpermutes and returns
    /// a populated header + remainder.
    #[test]
    fn parse_wire_slice_round_trip_v1_size1() {
        // Build an unpermuted body that contains a v1 I-frame slice
        // header + 3 trailing macroblock-layer bytes.
        let header_bits = pack(&[(3, 0b011), (1, 0), (8, 7), (5, 4), (1, 0), (1, 0), (1, 0)]);
        let mut unpermuted_body = header_bits.clone();
        unpermuted_body.extend_from_slice(&[0xAA, 0xBB, 0xCC]);

        // slice_size_size = 1 → no permutation; slice_size = body
        // length; version = 1 → prefix = (1 << 5) | 1 = 0x21.
        let slice_size = unpermuted_body.len() as u8;
        let mut wire = vec![(1 << 5) | 1, slice_size];
        wire.extend_from_slice(&unpermuted_body);

        let (hdr, rem) = parse_wire_slice(&wire, 99, false).unwrap();
        assert_eq!(hdr.version, SliceVersion::V1);
        assert_eq!(hdr.slice_size_size, 1);
        assert_eq!(hdr.slice_size, unpermuted_body.len() as u32);
        assert_eq!(hdr.frame_type, Svq3FrameType::Intra);
        assert_eq!(hdr.frame_number, 7);
        assert_eq!(hdr.slice_qp, 4);
        // header_end_bit ÷ 8 = full bytes the header occupies. The
        // header bit-length is 3+1+8+5+1+1+1 = 20 → 2 bytes + 4
        // bits, so remainder starts at byte offset 2.
        assert_eq!(hdr.header_end_bit, 20);
        assert_eq!(
            rem,
            vec![
                0x00, /* low 4 bits of header byte 3 = 0 */
                0xAA, 0xBB, 0xCC
            ]
        );
    }

    /// `parse_wire_slice` with slice_size_size = 2 exercises the
    /// permutation path.
    #[test]
    fn parse_wire_slice_round_trip_v2_size2() {
        // num_mbs = 99 ⇒ v2 mb-offset width = 7 bits.
        let header_bits = pack(&[
            (1, 0b1), // ue(0) = P
            (7, 50),  // mb offset
            (8, 11),  // frame number
            (5, 31),  // qp
            (1, 1),   // delta qp
            (1, 0),   // unknown
            (1, 0),   // optional loop: stop
        ]);
        // 1+7+8+5+1+1+1 = 24 bits = 3 bytes exact.
        let mut unpermuted_body = header_bits.clone();
        unpermuted_body.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        // Permute: slice_size_size = 2 ⇒ first 1 byte of the
        // unpermuted body was "moved to the very end of the slice"
        // (wiki spec §"Slice Header"). On-wire body =
        // unpermuted[1..] ++ unpermuted[..1].
        let permuted_body: Vec<u8> = {
            let mut v = unpermuted_body[1..].to_vec();
            v.extend_from_slice(&unpermuted_body[..1]);
            v
        };
        // Sanity check: unpermute(permuted) == unpermuted.
        let round_trip = unpermute_slice_payload(&permuted_body, 2).unwrap();
        assert_eq!(round_trip, unpermuted_body);

        // Wire: prefix = (2 << 5) | 2 = 0x42, slice_size = body len
        // as 2 BE bytes.
        let slice_size = permuted_body.len() as u16;
        let mut wire = vec![(2 << 5) | 2];
        wire.extend_from_slice(&slice_size.to_be_bytes());
        wire.extend_from_slice(&permuted_body);

        let (hdr, _rem) = parse_wire_slice(&wire, 99, false).unwrap();
        assert_eq!(hdr.version, SliceVersion::V2);
        assert_eq!(hdr.slice_size_size, 2);
        assert_eq!(hdr.slice_size, permuted_body.len() as u32);
        assert_eq!(hdr.frame_type, Svq3FrameType::Predicted);
        assert_eq!(hdr.mb_offset_v2, Some(50));
        assert_eq!(hdr.frame_number, 11);
        assert_eq!(hdr.slice_qp, 31);
        assert!(hdr.delta_qp_present);
    }

    #[test]
    fn parse_wire_slice_rejects_frame_end_sentinel() {
        // 0xFF at the start of `wire_slice` is the wiki spec's frame-
        // data end signal.
        let wire = [SVQ3_FRAME_END];
        assert!(matches!(
            parse_wire_slice(&wire, 99, false),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn parse_wire_slice_rejects_unknown_version() {
        // prefix bits 0-4 = version. Versions 1 and 2 are known; 3 is
        // not.
        let prefix = (1 << 5) | 3;
        let wire = [prefix, 0x01, 0x00];
        let err = parse_wire_slice(&wire, 99, false).unwrap_err();
        assert!(matches!(err, Error::InvalidFrameCode(3)));
    }

    #[test]
    fn parse_wire_slice_rejects_size_size_0() {
        // prefix high 3 bits = 0 → invalid size-size. (Version 1 in
        // low 5 bits so the version check passes before size-size is
        // inspected.)
        let prefix: u8 = 1;
        let wire = [prefix, 0xAA, 0xBB];
        assert!(matches!(
            parse_wire_slice(&wire, 99, false),
            Err(Error::BadBitWidth(0))
        ));
    }

    #[test]
    fn parse_wire_slice_rejects_truncated_size_field() {
        // size_size = 3, but only 2 bytes of size follow.
        let prefix = (3 << 5) | 1;
        let wire = [prefix, 0x00, 0x01];
        assert!(matches!(
            parse_wire_slice(&wire, 99, false),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn parse_wire_slice_rejects_truncated_body() {
        // Prefix says size = 100, but only 5 bytes of body.
        let prefix = (1 << 5) | 1;
        let wire = [prefix, 100, 0x00, 0x00, 0x00, 0x00];
        assert!(matches!(
            parse_wire_slice(&wire, 99, false),
            Err(Error::Truncated)
        ));
    }
}
