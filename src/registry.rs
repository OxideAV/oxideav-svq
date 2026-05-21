//! `oxideav-core` integration layer for `oxideav-svq`.
//!
//! Gated behind the default-on `registry` feature. Wires up:
//!
//! * `From<crate::Error> for oxideav_core::Error` — surfacing the
//!   SVQ1-specific structural failures as framework-level errors.
//! * The [`Decoder`] trait implementation. Round 2 walks the SVQ1
//!   frame header (per
//!   `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
//!   Header"), confirms it's structurally valid, and exposes the
//!   decoded width / height / picture-type on demand via the
//!   [`Svq1DecoderHandle::last_header`] accessor. `receive_frame`
//!   itself returns [`oxideav_core::Error::Unsupported`] because the
//!   encoded plane data of the wiki spec's §"Decoding Intraframe Plane
//!   Data" / §"Decoding Interframe Plane Data" depends on the
//!   multi-stage VQ codebooks + per-level VLC tables enumerated in
//!   "Appendix A: SVQ1 Data Tables", which are not yet pinned in
//!   `docs/` (tracked by the codebook docs-collaborator task — confirmed
//!   blocked at round-2 dispatch).
//! * A probe that confirms a candidate FourCC's first packet starts
//!   with a structurally-valid SVQ1 frame header.
//! * `register_codecs` / `register` entry points the umbrella
//!   `oxideav` crate calls during framework initialisation, plus the
//!   `__oxideav_entry` symbol the `register!` macro expands to.

use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry, CodecTag, Decoder,
    Error, Frame, Packet, ProbeContext, Result, RuntimeContext,
};

use crate::header::{parse_frame_header, Svq1FrameHeader};
use crate::CODEC_ID_STR;

// ---- Error conversion --------------------------------------------------

impl From<crate::Error> for Error {
    fn from(e: crate::Error) -> Self {
        match e {
            crate::Error::InvalidFrameCode(code) => Error::InvalidData(format!(
                "oxideav-svq: invalid SVQ1 frame code 0x{code:06x} (bits 6 and 5 both clear)"
            )),
            crate::Error::InvalidPictureType => Error::InvalidData(
                "oxideav-svq: SVQ1 picture-type field decoded to the reserved value 3".into(),
            ),
            crate::Error::InvalidChecksumTrailer => Error::InvalidData(
                "oxideav-svq: SVQ1 checksum-trailer unknown_field_1 must be zero per spec".into(),
            ),
            crate::Error::Truncated => Error::InvalidData(
                "oxideav-svq: SVQ1 chunk truncated mid frame-header field".into(),
            ),
            crate::Error::BadBitWidth(n) => {
                Error::other(format!("oxideav-svq: bit-reader rejected width {n}"))
            }
            crate::Error::NotImplemented => Error::unsupported(
                "oxideav-svq: SVQ1 pixel decode blocked on codebook docs-gap — see crates/oxideav-svq/README.md",
            ),
        }
    }
}

// ---- Registry entry points ---------------------------------------------

/// Register the SVQ1 (`svq1` / `SVQ1` / `svqi`) decoder into the
/// supplied [`CodecRegistry`].
///
/// FourCC list is sourced from
/// `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §header metadata
/// (line 9: "FOURCCs: svq1, SVQ1, svqi"). `CodecTag::fourcc`
/// upper-cases the bytes internally so `svq1` and `SVQ1` both
/// canonicalise to the same FourCC; `svqi` is registered separately.
pub fn register_codecs(reg: &mut CodecRegistry) {
    // Per the wiki spec SVQ1 carries an SVQ1-bitstream key (intra)
    // frame plus delta (P) and droppable (B) frames; not intra-only.
    // Max-size cap is generous — the wiki spec lists 704×576 as the
    // largest standard size but escape-code-7 allows arbitrary 12-bit
    // width / height (≤ 4095×4095). Use 4096×4096 as a defensive cap.
    let caps = CodecCapabilities::video("svq1_sw")
        .with_lossy(true)
        .with_intra_only(false)
        .with_max_size(4096, 4096);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .probe(probe_svq1)
            .tags([CodecTag::fourcc(b"SVQ1"), CodecTag::fourcc(b"svqi")]),
    );
}

/// Disambiguating probe for the SVQ1 FourCCs.
///
/// When a packet is present, confirms the first 32 bits of the chunk
/// decode as a structurally-valid SVQ1 frame header (frame-code
/// invariant + picture-type ≠ reserved). When no packet is available,
/// returns `0.5` — the SVQ1 FourCCs (`svq1` / `SVQ1` / `svqi`) are not
/// shared with any other codec currently in the registry, so weak
/// evidence still outweighs the null claim.
pub fn probe_svq1(ctx: &ProbeContext) -> f32 {
    let Some(pkt) = ctx.packet else {
        return 0.5;
    };
    // The wiki spec's frame header needs at least 32 bits before any
    // optional sub-fields kick in (22 frame_code + 8 temporal_reference
    // + 2 picture_type). A packet shorter than 4 bytes can never carry
    // a complete header.
    if pkt.len() < 4 {
        return 0.0;
    }
    // parse_frame_header walks the whole header; for a probe we only
    // care that it accepts the first 32 bits. The variable-length
    // I-frame trailer can fail with `Truncated` on a deliberately-
    // short header peek — accept that as evidence the first 32 bits
    // parsed OK by inspecting which error was returned.
    match parse_frame_header(pkt) {
        Ok(_) => 1.0,
        Err(crate::Error::Truncated) => 0.5,
        Err(_) => 0.0,
    }
}

/// Unified entry point: install the SVQ1 codec into a
/// [`RuntimeContext`].
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
}

oxideav_core::register!("svq1", register);

// ---- Decoder trait impl ------------------------------------------------

/// Factory function the framework calls to instantiate a fresh SVQ1
/// decoder. Stores the requested [`CodecId`] so [`Decoder::codec_id`]
/// can return a stable borrow.
pub fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    let codec_id = params.codec_id.clone();
    Ok(Box::new(Svq1DecoderHandle {
        codec_id,
        pending: None,
        last_header: None,
        eof: false,
    }))
}

/// SVQ1 decoder handle bound to a single stream.
///
/// Round 2 implements the `send_packet` → frame-header-parse path: a
/// successful `send_packet` records the parsed header in
/// [`Self::last_header`] (accessible to integrators that need the
/// dimensions before pixel data is available) and `receive_frame`
/// returns [`Error::Unsupported`] because the encoded plane data is
/// blocked on the codebook docs-gap. The handle deliberately does NOT
/// silently drop packets — the framework contract requires
/// `send_packet → receive_frame` to be paired, so a packet that
/// parses structurally is held until `receive_frame` is called and
/// the (currently `Unsupported`) error is delivered to the caller.
pub struct Svq1DecoderHandle {
    codec_id: CodecId,
    pending: Option<Packet>,
    last_header: Option<Svq1FrameHeader>,
    eof: bool,
}

impl std::fmt::Debug for Svq1DecoderHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Svq1DecoderHandle")
            .field("codec_id", &self.codec_id)
            .field("pending", &self.pending.as_ref().map(|p| p.data.len()))
            .field("last_header", &self.last_header)
            .field("eof", &self.eof)
            .finish()
    }
}

impl Svq1DecoderHandle {
    /// Returns the most recently parsed SVQ1 frame header, if any.
    ///
    /// Populated by [`Decoder::send_packet`] on every structurally
    /// valid packet. Lets integrators read the dimensions / picture
    /// type before the (currently unsupported) pixel-data decode lands.
    pub fn last_header(&self) -> Option<&Svq1FrameHeader> {
        self.last_header.as_ref()
    }
}

impl Decoder for Svq1DecoderHandle {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        if self.pending.is_some() {
            return Err(Error::other(
                "oxideav-svq: receive_frame must be called before sending another packet",
            ));
        }
        // Parse the header eagerly so the framework gets early
        // feedback on malformed streams. The header is recorded for
        // accessor use; the packet itself is retained in `pending` so
        // `receive_frame` can surface the (currently unsupported)
        // pixel-decode error from the right call site.
        let header = parse_frame_header(&packet.data)?;
        self.last_header = Some(header);
        self.pending = Some(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        if self.pending.take().is_none() {
            return if self.eof {
                Err(Error::Eof)
            } else {
                Err(Error::NeedMore)
            };
        }
        // The frame header parsed OK in send_packet — pixel decode is
        // the part that needs the codebook tables we don't have yet.
        Err(Error::unsupported(
            "oxideav-svq: SVQ1 pixel decode blocked on codebook docs-gap — see crates/oxideav-svq/README.md",
        ))
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.pending = None;
        self.last_header = None;
        self.eof = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::Svq1PictureType;
    use oxideav_core::TimeBase;

    /// Helper: pack a sequence of `(width, value)` items into a byte
    /// stream by writing them MSB-first. Mirrors the helper used by
    /// `header::tests` so probe + decoder tests can build fixtures
    /// without re-implementing the bit-packing.
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

    fn minimal_iframe_packet() -> Vec<u8> {
        // I-frame with frame_code=0x40, picture-type=I, frame-size-code=2
        // (176×144). Same layout as the `iframe_size_code_2_minimal`
        // test in header.rs.
        pack(&[
            (22, 0x40),
            (8, 0),
            (2, 0),
            (2, 0),
            (2, 0),
            (1, 0),
            (3, 2),
            (1, 0),
            (1, 0),
        ])
    }

    #[test]
    fn registers_svq1_codec_id() {
        let mut ctx = RuntimeContext::new();
        super::register(&mut ctx);
        assert!(ctx.codecs.decoder_ids().any(|id| id.as_str() == "svq1"));
    }

    #[test]
    fn probe_accepts_valid_iframe_header() {
        let pkt = minimal_iframe_packet();
        let tag = CodecTag::fourcc(b"SVQ1");
        let ctx = ProbeContext::new(&tag).packet(&pkt);
        assert_eq!(probe_svq1(&ctx), 1.0);
    }

    #[test]
    fn probe_rejects_bits_65_clear_frame_code() {
        // frame_code=0x80 has bit 7 set but bits 6 & 5 clear — invalid
        // per the wiki spec's `(frame_code & 0x60) == 0` rule.
        let bytes = pack(&[(22, 0x80), (8, 0), (2, 0)]);
        let tag = CodecTag::fourcc(b"SVQ1");
        let ctx = ProbeContext::new(&tag).packet(&bytes);
        assert_eq!(probe_svq1(&ctx), 0.0);
    }

    #[test]
    fn probe_rejects_picture_type_3() {
        // frame_code=0x6F (valid), picture-type=3 (reserved/invalid).
        let bytes = pack(&[(22, 0x6F), (8, 0), (2, 3)]);
        let tag = CodecTag::fourcc(b"SVQ1");
        let ctx = ProbeContext::new(&tag).packet(&bytes);
        assert_eq!(probe_svq1(&ctx), 0.0);
    }

    #[test]
    fn probe_returns_partial_confidence_without_packet() {
        let tag = CodecTag::fourcc(b"SVQ1");
        let ctx = ProbeContext::new(&tag);
        assert_eq!(probe_svq1(&ctx), 0.5);
    }

    #[test]
    fn probe_short_packet_returns_zero() {
        let tag = CodecTag::fourcc(b"SVQ1");
        let ctx = ProbeContext::new(&tag).packet(&[0xFF, 0xFF]);
        assert_eq!(probe_svq1(&ctx), 0.0);
    }

    /// Build a `Packet` carrying the supplied SVQ1 bitstream bytes,
    /// stream-index 0 / millisecond timebase / no pts. Wrapper so the
    /// test bodies don't have to repeat the 3-arg `Packet::new`
    /// signature each time.
    fn make_packet(data: Vec<u8>) -> Packet {
        Packet::new(0, TimeBase::new(1, 1_000), data)
    }

    #[test]
    fn decoder_send_packet_then_receive_returns_unsupported() {
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut decoder = make_decoder(&params).expect("make_decoder ok");
        let pkt = make_packet(minimal_iframe_packet());
        decoder.send_packet(&pkt).expect("send_packet ok");
        let err = decoder
            .receive_frame()
            .expect_err("pixel decode is unsupported until codebooks land");
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn decoder_rejects_malformed_packet() {
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut decoder = make_decoder(&params).expect("make_decoder ok");
        // frame_code=0x80 is structurally invalid per the spec.
        let bad = pack(&[(22, 0x80), (8, 0), (2, 0)]);
        let err = decoder
            .send_packet(&make_packet(bad))
            .expect_err("malformed packet must be rejected at send_packet");
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn decoder_receive_frame_without_packet_signals_need_more() {
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut decoder = make_decoder(&params).expect("make_decoder ok");
        let err = decoder
            .receive_frame()
            .expect_err("no pending packet — must signal NeedMore");
        assert!(matches!(err, Error::NeedMore));
    }

    #[test]
    fn decoder_flush_then_receive_signals_eof() {
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut decoder = make_decoder(&params).expect("make_decoder ok");
        decoder.flush().expect("flush ok");
        let err = decoder
            .receive_frame()
            .expect_err("post-flush drain — must signal Eof");
        assert!(matches!(err, Error::Eof));
    }

    #[test]
    fn decoder_send_packet_twice_without_drain_errors() {
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut decoder = make_decoder(&params).expect("make_decoder ok");
        let pkt = make_packet(minimal_iframe_packet());
        decoder.send_packet(&pkt).expect("first send_packet ok");
        let err = decoder
            .send_packet(&pkt)
            .expect_err("second send_packet without receive_frame must reject");
        assert!(matches!(err, Error::Other(_)));
    }

    #[test]
    fn handle_last_header_exposes_parsed_dimensions() {
        // Direct construction (bypassing make_decoder's Box) so the
        // concrete handle's `last_header` accessor is exercised.
        let mut handle = Svq1DecoderHandle {
            codec_id: CodecId::new(CODEC_ID_STR),
            pending: None,
            last_header: None,
            eof: false,
        };
        let pkt = make_packet(minimal_iframe_packet());
        handle.send_packet(&pkt).expect("send_packet ok");
        let hdr = handle.last_header().expect("header recorded");
        assert_eq!(hdr.width, Some(176));
        assert_eq!(hdr.height, Some(144));
        assert_eq!(hdr.picture_type, Svq1PictureType::Intra);
    }

    #[test]
    fn error_conversion_carries_structural_failure_messages() {
        let e: Error = crate::Error::InvalidPictureType.into();
        match e {
            Error::InvalidData(msg) => assert!(msg.contains("picture-type")),
            other => panic!("unexpected variant: {other:?}"),
        }

        let e: Error = crate::Error::NotImplemented.into();
        assert!(matches!(e, Error::Unsupported(_)));
    }
}
