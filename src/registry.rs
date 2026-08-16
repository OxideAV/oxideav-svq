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
    Encoder, Error, Frame, Packet, ProbeContext, Result, RuntimeContext,
};

use crate::header::{parse_frame_header, Svq1FrameHeader};
use crate::svq3::{parse_extradata, parse_wire_slice, Svq3SequenceHeader, Svq3SliceHeader};
use crate::{CODEC_ID_STR, SVQ3_CODEC_ID_STR};

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
            crate::Error::InvalidIntraPrediction(top, left, idx) => Error::InvalidData(format!(
                "oxideav-svq: SVQ3 intra-4x4 prediction lookup landed on -1 sentinel \
                 at INTRA_PRED_TABLE[{top}][{left}][{idx}]"
            )),
            crate::Error::InvalidLevelQuantise(level) => Error::InvalidData(format!(
                "oxideav-svq: SVQ1 in-place quantise decision at {level:?} is illegal — \
                 no codebook stored at this level (spec §14.10/§14.11)"
            )),
            crate::Error::InvalidVlcCode => Error::InvalidData(
                "oxideav-svq: SVQ1 bit pattern matched no codeword in the addressed VLC table"
                    .into(),
            ),
            crate::Error::UnexpectedIntraSkip => Error::InvalidData(
                "oxideav-svq: SVQ1 stage-count VLC decoded to SKIP on an intra-coded leaf"
                    .into(),
            ),
            crate::Error::ReconstructFailed => Error::InvalidData(
                "oxideav-svq: SVQ1 leaf-block reconstruction failed".into(),
            ),
            crate::Error::MissingReference => Error::InvalidData(
                "oxideav-svq: SVQ1 P/B frame requires the previous I- or P-frame as reference"
                    .into(),
            ),
            crate::Error::NotImplemented => Error::unsupported(
                "oxideav-svq: SVQ1 pixel decode blocked on codebook docs-gap — see crates/oxideav-svq/README.md",
            ),
            crate::Error::InvalidQuantiser(q) => Error::InvalidData(format!(
                "oxideav-svq: SVQ3 quantiser delta drove the macroblock quantiser to {q} \
                 (outside the dequantisation ladder domain 0..=31)"
            )),
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
            .encoder(make_encoder)
            .probe(probe_svq1)
            .tags([CodecTag::fourcc(b"SVQ1"), CodecTag::fourcc(b"svqi")]),
    );

    // SVQ3: round 3 only wires the framework hook — the decoder still
    // returns `Unsupported` on `receive_frame` because the macroblock
    // layer is out of round-3 scope. Per
    // `docs/video/svq3/wiki/Sorenson_Video_3.wiki` line 7 the only
    // FourCC SVQ3 is registered with is `SVQ3`; the same 12-bit
    // explicit width/height escape and structural max-size policy as
    // SVQ1 applies.
    let svq3_caps = CodecCapabilities::video("svq3_sw")
        .with_lossy(true)
        .with_intra_only(false)
        .with_max_size(4096, 4096);
    reg.register(
        CodecInfo::new(CodecId::new(SVQ3_CODEC_ID_STR))
            .capabilities(svq3_caps)
            .decoder(make_svq3_decoder)
            .probe(probe_svq3)
            .tags([CodecTag::fourcc(b"SVQ3")]),
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
        reference: None,
        eof: false,
    }))
}

/// SVQ1 decoder handle bound to a single stream.
///
/// `send_packet` parses + validates the frame header (recorded in
/// [`Self::last_header`]) and retains the packet; `receive_frame`
/// runs the full pixel decode ([`crate::svq1_plane::decode_frame`])
/// against the held reference picture and returns a `Yuv420P`
/// [`oxideav_core::VideoFrame`] (SVQ1's native 4:1:0 chroma is
/// nearest-neighbour-bridged to 4:2:0 — see
/// [`crate::svq1_plane::Svq1DecodedFrame::to_video_frame_420`]; the
/// native planes are reachable through the standalone
/// `svq1_plane` API). I- and P-frames become the reference for the
/// next frame; B ("droppable") frames never do (wiki §"Algorithm
/// Basics" — other frames cannot predict from them).
pub struct Svq1DecoderHandle {
    codec_id: CodecId,
    pending: Option<Packet>,
    last_header: Option<Svq1FrameHeader>,
    reference: Option<crate::svq1_plane::Svq1DecodedFrame>,
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
        let Some(packet) = self.pending.take() else {
            return if self.eof {
                Err(Error::Eof)
            } else {
                Err(Error::NeedMore)
            };
        };
        let decoded = crate::svq1_plane::decode_frame(&packet.data, self.reference.as_ref())?;
        let video = decoded.to_video_frame_420(packet.pts);
        // I- and P-frames become the reference for subsequent frames;
        // B ("droppable") frames never do (wiki §"Algorithm Basics").
        if decoded.header.picture_type != crate::header::Svq1PictureType::Droppable {
            self.reference = Some(decoded);
        }
        Ok(Frame::Video(video))
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.pending = None;
        self.last_header = None;
        self.reference = None;
        self.eof = false;
        Ok(())
    }
}

// ---- Encoder trait impl --------------------------------------------------

/// Factory function the framework calls to instantiate a fresh SVQ1
/// encoder. Requires video parameters with `width` / `height` set
/// (≤ 4095 each — the spec/01 12-bit explicit-dimension ceiling).
///
/// Input frames are `Yuv420P` [`oxideav_core::VideoFrame`]s (the
/// bridge layout the decoder side emits); the handle
/// nearest-neighbour-decimates chroma back onto SVQ1's native 4:1:0
/// lattice (spec/02 §2.2) — the exact inverse of
/// [`crate::svq1_plane::Svq1DecodedFrame::to_video_frame_420`]'s
/// doubling. The first frame (and every
/// [`Svq1EncoderHandle::keyframe_interval`]-th after it) is coded
/// intra via the λ-tree ([`crate::svq1_enc_tree`]); the rest are
/// P-frames through [`crate::svq1_enc_inter::encode_inter_frame`],
/// chained on the decoder-authoritative reconstruction.
pub fn make_encoder(params: &CodecParameters) -> Result<Box<dyn Encoder>> {
    make_encoder_handle(params).map(|handle| Box::new(handle) as Box<dyn Encoder>)
}

/// [`make_encoder`] returning the concrete handle, so direct-API
/// callers can reach the tuning knobs ([`Svq1EncoderHandle::set_lambda`],
/// [`Svq1EncoderHandle::set_keyframe_interval`],
/// [`Svq1EncoderHandle::set_target_frame_bytes`]) before boxing.
pub fn make_encoder_handle(params: &CodecParameters) -> Result<Svq1EncoderHandle> {
    let width = params.width.unwrap_or(0) as usize;
    let height = params.height.unwrap_or(0) as usize;
    if width == 0 || height == 0 || width > 4095 || height > 4095 {
        return Err(Error::invalid(
            "oxideav-svq: encoder requires video dimensions in 1..=4095",
        ));
    }
    let mut output_params = CodecParameters::video(params.codec_id.clone());
    output_params.width = Some(width as u32);
    output_params.height = Some(height as u32);
    output_params.pixel_format = params.pixel_format;
    output_params.frame_rate = params.frame_rate;
    let output_params = output_params.with_tag(CodecTag::fourcc(b"SVQ1"));
    Ok(Svq1EncoderHandle {
        codec_id: params.codec_id.clone(),
        output_params,
        width,
        height,
        lambda: 32,
        keyframe_interval: 30,
        target_frame_bytes: None,
        rc_lambda: 32,
        droppable_period: 0,
        frame_index: 0,
        reference: None,
        pending: std::collections::VecDeque::new(),
        eof: false,
    })
}

/// SVQ1 encoder handle bound to a single stream. See [`make_encoder`].
pub struct Svq1EncoderHandle {
    codec_id: CodecId,
    output_params: CodecParameters,
    width: usize,
    height: usize,
    /// Rate weight shared by the intra λ-tree and the inter mode
    /// decision (SSE units per wire bit).
    lambda: u64,
    /// Distance between intra frames; the first frame is always intra.
    keyframe_interval: u32,
    /// Per-frame byte budget for the λ rate controller
    /// ([`Self::set_target_frame_bytes`]); `None` = fixed-λ encoding.
    target_frame_bytes: Option<usize>,
    /// The rate controller's warm-start λ (last chosen value).
    rc_lambda: u64,
    /// Droppable-frame cadence ([`Self::set_droppable_period`]);
    /// `0` = no droppable frames.
    droppable_period: u32,
    frame_index: u32,
    reference: Option<crate::svq1_plane::Svq1DecodedFrame>,
    pending: std::collections::VecDeque<Packet>,
    eof: bool,
}

impl std::fmt::Debug for Svq1EncoderHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Svq1EncoderHandle")
            .field("codec_id", &self.codec_id)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("lambda", &self.lambda)
            .field("keyframe_interval", &self.keyframe_interval)
            .field("frame_index", &self.frame_index)
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl Svq1EncoderHandle {
    /// Set the rate weight λ (SSE units per wire bit; `0` maximises
    /// fidelity). Direct-API knob — the factory default is `32`.
    pub fn set_lambda(&mut self, lambda: u64) {
        self.lambda = lambda;
    }

    /// Set the intra-frame cadence (`1` = all-intra). Direct-API knob
    /// — the factory default is `30`.
    pub fn set_keyframe_interval(&mut self, interval: u32) {
        self.keyframe_interval = interval.max(1);
    }

    /// Enable (`Some(bytes)`) or disable (`None`) the per-frame rate
    /// controller. Direct-API knob — the factory default is `None`
    /// (fixed-λ encoding at [`Self::set_lambda`]'s value).
    ///
    /// When enabled, each frame is encoded at the SMALLEST λ (highest
    /// fidelity) whose codec-frame byte size fits the budget, found by
    /// a deterministic warm-started doubling + bisection over the
    /// λ-tree encoder (λ is a free encoder-side policy knob — the wire
    /// format carries no rate state, so any λ schedule yields
    /// conformant streams; validated by the round-trip tests). If even
    /// the λ ceiling (`2^20`) cannot fit the budget the frame is
    /// emitted best-effort at the ceiling — SVQ1 has a per-MB wire
    /// floor (T03 / block-tree codewords) that no λ can compress away.
    pub fn set_target_frame_bytes(&mut self, target: Option<usize>) {
        self.target_frame_bytes = target;
        self.rc_lambda = self.lambda;
    }

    /// Set the droppable (B) frame cadence. Direct-API knob — the
    /// factory default is `0` (no droppable frames).
    ///
    /// With `period > 0`, every inter frame whose index is NOT a
    /// multiple of `period` is emitted as an SVQ1 droppable frame
    /// (picture type 2) — e.g. `period = 2` produces `I B P B P …`.
    /// Droppable frames predict from the last NON-droppable frame and
    /// never become the reference (wiki §"Algorithm Basics" / spec/06
    /// §6.10 item 8), so a transport may discard their packets
    /// without perturbing any later frame — reference transparency is
    /// CI-pinned at the registry level.
    pub fn set_droppable_period(&mut self, period: u32) {
        self.droppable_period = period;
    }

    /// Nearest-neighbour 4:2:0 → 4:1:0 chroma decimation (the exact
    /// inverse of the decoder bridge's 2×2 doubling).
    fn decimate_chroma(&self, plane: &oxideav_core::VideoPlane) -> Vec<u8> {
        let cw20 = self.width.div_ceil(2);
        let ch20 = self.height.div_ceil(2);
        let cw10 = crate::svq1_plane::chroma_dim(self.width);
        let ch10 = crate::svq1_plane::chroma_dim(self.height);
        let mut out = Vec::with_capacity(cw10 * ch10);
        for row in 0..ch10 {
            let src_row = (row * 2).min(ch20 - 1);
            for col in 0..cw10 {
                let src_col = (col * 2).min(cw20 - 1);
                out.push(plane.data[src_row * plane.stride + src_col]);
            }
        }
        out
    }
}

impl Encoder for Svq1EncoderHandle {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }

    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let Frame::Video(video) = frame else {
            return Err(Error::invalid("oxideav-svq: encoder expects video frames"));
        };
        if video.planes.len() != 3 {
            return Err(Error::invalid(
                "oxideav-svq: encoder expects 3-plane Yuv420P input",
            ));
        }
        let (cw20, ch20) = (self.width.div_ceil(2), self.height.div_ceil(2));
        let y_plane = &video.planes[0];
        if y_plane.stride < self.width
            || y_plane.data.len() < y_plane.stride * self.height
            || video.planes[1].stride < cw20
            || video.planes[1].data.len() < video.planes[1].stride * ch20
            || video.planes[2].stride < cw20
            || video.planes[2].data.len() < video.planes[2].stride * ch20
        {
            return Err(Error::invalid("oxideav-svq: input plane geometry mismatch"));
        }

        // Tightly pack the luma plane (drop any stride slack).
        let y: Vec<u8> = (0..self.height)
            .flat_map(|row| {
                y_plane.data[row * y_plane.stride..row * y_plane.stride + self.width]
                    .iter()
                    .copied()
            })
            .collect();
        let u = self.decimate_chroma(&video.planes[1]);
        let v = self.decimate_chroma(&video.planes[2]);
        let (cw, ch) = (
            crate::svq1_plane::chroma_dim(self.width),
            crate::svq1_plane::chroma_dim(self.height),
        );
        let yr = crate::svq1_enc::Svq1PlaneRef {
            samples: &y,
            width: self.width,
            height: self.height,
        };
        let ur = crate::svq1_enc::Svq1PlaneRef {
            samples: &u,
            width: cw,
            height: ch,
        };
        let vr = crate::svq1_enc::Svq1PlaneRef {
            samples: &v,
            width: cw,
            height: ch,
        };

        let intra_due =
            self.reference.is_none() || self.frame_index % self.keyframe_interval.max(1) == 0;
        // Droppable (B) cadence: inter frames whose index is not a
        // multiple of the period are emitted as picture type 2 and
        // never become the reference (see `set_droppable_period`).
        let droppable = !intra_due
            && self.droppable_period > 0
            && self.frame_index % self.droppable_period != 0;
        let temporal_reference = (self.frame_index & 0xff) as u8;
        let encode_once = |lambda: u64| -> Result<(Vec<u8>, crate::svq1_plane::Svq1DecodedFrame)> {
            if intra_due {
                let bytes = crate::svq1_enc::encode_intra_frame(
                    yr,
                    ur,
                    vr,
                    crate::svq1_enc::Svq1EncoderMode::Adaptive { lambda },
                )?;
                let recon = crate::svq1_plane::decode_intra_frame(&bytes)?;
                Ok((bytes, recon))
            } else {
                let reference = self.reference.as_ref().expect("reference present");
                let encoded = crate::svq1_enc_inter::encode_inter_frame(
                    yr,
                    ur,
                    vr,
                    reference,
                    &crate::svq1_enc_inter::Svq1InterParams {
                        lambda,
                        temporal_reference,
                        droppable,
                        ..Default::default()
                    },
                )?;
                Ok((encoded.bytes, encoded.reconstruction))
            }
        };

        let (bytes, recon, chosen_lambda) = match self.target_frame_bytes {
            None => {
                let (bytes, recon) = encode_once(self.lambda)?;
                (bytes, recon, self.lambda)
            }
            Some(target) => {
                // Smallest λ whose output fits `target`, by warm-started
                // doubling + bisection (size decreases with λ; occasional
                // local non-monotonicity only costs optimality of the
                // bracket endpoint, never the ≤-target guarantee).
                const LAMBDA_CEILING: u64 = 1 << 20;
                let warm = self.rc_lambda.min(LAMBDA_CEILING);
                let mut best = encode_once(warm)?;
                let mut best_lambda = warm;
                if best.0.len() <= target {
                    // Fits — chase fidelity downward. λ = 0 first (the
                    // common generous-budget exit), then bisect.
                    let (zero_bytes, zero_recon) = encode_once(0)?;
                    if zero_bytes.len() <= target {
                        best = (zero_bytes, zero_recon);
                        best_lambda = 0;
                    } else {
                        let mut lo = 0u64; // known not to fit
                        let mut hi = best_lambda; // known to fit
                        while hi - lo > 1 {
                            let mid = lo + (hi - lo) / 2;
                            let candidate = encode_once(mid)?;
                            if candidate.0.len() <= target {
                                best = candidate;
                                best_lambda = mid;
                                hi = mid;
                            } else {
                                lo = mid;
                            }
                        }
                    }
                } else {
                    // Too big — double upward until it fits (or hit the
                    // ceiling: best-effort emission), then bisect back.
                    let mut lo = best_lambda; // known not to fit
                    let mut hi = best_lambda.max(1) * 2;
                    loop {
                        let candidate = encode_once(hi.min(LAMBDA_CEILING))?;
                        let fits = candidate.0.len() <= target;
                        if fits {
                            best = candidate;
                            best_lambda = hi.min(LAMBDA_CEILING);
                            break;
                        }
                        if hi >= LAMBDA_CEILING {
                            // Best effort: the ceiling encode is the
                            // smallest this encoder can produce.
                            best = candidate;
                            best_lambda = LAMBDA_CEILING;
                            break;
                        }
                        lo = hi;
                        hi *= 2;
                    }
                    if best.0.len() <= target {
                        let mut hi = best_lambda; // fits
                        while hi - lo > 1 {
                            let mid = lo + (hi - lo) / 2;
                            let candidate = encode_once(mid)?;
                            if candidate.0.len() <= target {
                                best = candidate;
                                best_lambda = mid;
                                hi = mid;
                            } else {
                                lo = mid;
                            }
                        }
                    }
                }
                let (bytes, recon) = best;
                (bytes, recon, best_lambda)
            }
        };
        let keyframe = intra_due;
        self.rc_lambda = chosen_lambda;
        if !droppable {
            self.reference = Some(recon);
        }
        self.frame_index = self.frame_index.wrapping_add(1);

        let mut packet =
            Packet::new(0, oxideav_core::TimeBase::MICROS, bytes).with_keyframe(keyframe);
        packet.pts = video.pts;
        packet.dts = video.pts;
        self.pending.push_back(packet);
        Ok(())
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        match self.pending.pop_front() {
            Some(packet) => Ok(packet),
            None if self.eof => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }
}

// ---- SVQ3 probe + decoder ----------------------------------------------

/// Disambiguating probe for the SVQ3 `SVQ3` FourCC.
///
/// SVQ3 differs from SVQ1 in that it has **no** generic in-stream
/// frame header at the start of each packet — packets are slices
/// whose first byte is either the `0xFF` frame-end sentinel or a
/// 1-byte version/size-size prefix where the low 5 bits hold the
/// slice-header version (only `1` or `2` is valid per the wiki
/// spec) and the high 3 bits hold the slice-size-size (range 1..=3).
/// The probe accepts a candidate packet iff its first byte satisfies
/// those structural invariants. When no packet is available the
/// probe returns `0.5` — `SVQ3` is the only FourCC in the current
/// registry that maps to this codec so the FourCC alone is strong
/// evidence.
pub fn probe_svq3(ctx: &ProbeContext) -> f32 {
    let Some(pkt) = ctx.packet else {
        return 0.5;
    };
    if pkt.is_empty() {
        return 0.0;
    }
    let prefix = pkt[0];
    if prefix == crate::svq3::SVQ3_FRAME_END {
        // A 0xFF byte at the start of a packet means "frame data
        // end" per the wiki spec. Not invalid — but doesn't carry a
        // slice header either. Treat as weak evidence.
        return 0.5;
    }
    let size_size = (prefix >> 5) & 0x07;
    let version = prefix & 0x1F;
    if !(1..=3).contains(&size_size) {
        return 0.0;
    }
    if version != 1 && version != 2 {
        return 0.0;
    }
    1.0
}

/// Factory function the framework calls to instantiate a fresh SVQ3
/// decoder.
pub fn make_svq3_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    let codec_id = params.codec_id.clone();
    // The SVQ3 sequence header lives in extradata; the factory
    // tries to parse it eagerly so dimension lookups + the v2 mb-
    // offset field width are available before the first packet
    // arrives. A missing or unparseable extradata is **not** fatal
    // at construction time — round 3 surfaces it via
    // `Svq3DecoderHandle::sequence_header()` returning `None` and
    // returning a structural error on the first `send_packet`.
    let sequence_header = if params.extradata.is_empty() {
        None
    } else {
        parse_svq3_extradata_flexible(&params.extradata)
    };
    Ok(Box::new(Svq3DecoderHandle {
        codec_id,
        sequence_header,
        pending: None,
        last_slice_header: None,
        eof: false,
    }))
}

/// Parse SVQ3 extradata that may carry the container-level `SMI `
/// wrapper in front of the `SEQH` record.
///
/// QuickTime files deliver the sequence header inside an `SMI ` sample
/// -entry extension atom — a 4-byte big-endian size plus the `SMI `
/// tag — ahead of the `SEQH` marker + length + payload
/// (`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Sequence Header";
/// the staged fixtures' `extradata.bin` files carry exactly this
/// shape). Accept both the bare `SEQH…` form and the wrapped form by
/// locating the marker.
fn parse_svq3_extradata_flexible(extradata: &[u8]) -> Option<crate::svq3::Svq3SequenceHeader> {
    let start = extradata
        .windows(crate::svq3::SVQ3_SEQH_MAGIC.len())
        .position(|w| w == crate::svq3::SVQ3_SEQH_MAGIC)?;
    parse_extradata(&extradata[start..]).ok()
}

/// SVQ3 decoder handle bound to a single stream.
///
/// Round 3 implements the structural parse path: `send_packet` walks
/// the slice's 1-byte prefix + 1-3 byte size field + permuted body,
/// reverses the permutation, then parses the slice header per the
/// wiki spec's §"Slice Header". The parsed [`Svq3SliceHeader`] is
/// recorded in [`Self::last_slice_header`]; `receive_frame` returns
/// [`Error::Unsupported`] because the macroblock-layer Golomb decode
/// is out of round-3 scope.
pub struct Svq3DecoderHandle {
    codec_id: CodecId,
    /// Parsed SEQH extradata, if it was supplied at construction
    /// time and parsed successfully.
    sequence_header: Option<Svq3SequenceHeader>,
    pending: Option<Packet>,
    last_slice_header: Option<Svq3SliceHeader>,
    eof: bool,
}

impl std::fmt::Debug for Svq3DecoderHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Svq3DecoderHandle")
            .field("codec_id", &self.codec_id)
            .field("sequence_header", &self.sequence_header)
            .field("pending", &self.pending.as_ref().map(|p| p.data.len()))
            .field("last_slice_header", &self.last_slice_header)
            .field("eof", &self.eof)
            .finish()
    }
}

impl Svq3DecoderHandle {
    /// Returns the parsed SVQ3 sequence header, if any was supplied
    /// via [`CodecParameters::extradata`] and successfully parsed at
    /// construction time.
    pub fn sequence_header(&self) -> Option<&Svq3SequenceHeader> {
        self.sequence_header.as_ref()
    }

    /// Returns the most recently parsed SVQ3 slice header, if any.
    ///
    /// Populated by [`Decoder::send_packet`] on every structurally
    /// valid packet. Lets integrators inspect the per-slice frame-
    /// type / quantiser / frame-number before the (currently
    /// unsupported) macroblock-layer decode lands.
    pub fn last_slice_header(&self) -> Option<&Svq3SliceHeader> {
        self.last_slice_header.as_ref()
    }
}

impl Decoder for Svq3DecoderHandle {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        if self.pending.is_some() {
            return Err(Error::other(
                "oxideav-svq: receive_frame must be called before sending another packet",
            ));
        }
        // Per the wiki spec a packet starting with the 0xFF sentinel
        // is a frame-data-end marker, not a slice. Surface it as a
        // structural EOF-style signal via NeedMore so the caller
        // knows the codec consumed it but no header was parsed.
        if !packet.data.is_empty() && packet.data[0] == crate::svq3::SVQ3_FRAME_END {
            self.pending = Some(packet.clone());
            return Ok(());
        }
        let num_mbs = self
            .sequence_header
            .as_ref()
            .map(crate::svq3::num_macroblocks)
            .unwrap_or(1);
        let protected = self
            .sequence_header
            .as_ref()
            .map(|s| s.protected)
            .unwrap_or(false);
        let (header, _remainder) = parse_wire_slice(&packet.data, num_mbs, protected)?;
        self.last_slice_header = Some(header);
        self.pending = Some(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        let Some(packet) = self.pending.take() else {
            return if self.eof {
                Err(Error::Eof)
            } else {
                Err(Error::NeedMore)
            };
        };
        // A pure frame-end-sentinel packet carries no picture.
        if !packet.data.is_empty() && packet.data[0] == crate::svq3::SVQ3_FRAME_END {
            return Err(Error::NeedMore);
        }
        let Some(seqh) = self.sequence_header.as_ref() else {
            return Err(Error::InvalidData(
                "oxideav-svq: SVQ3 decode requires the SEQH sequence header in extradata".into(),
            ));
        };
        match self.last_slice_header.as_ref().map(|h| h.frame_type) {
            Some(crate::svq3::Svq3FrameType::Intra) => {
                let picture = crate::svq3_frame::decode_intra_access_unit(seqh, &packet.data)?;
                let video = picture.to_video_frame_cropped(
                    usize::from(seqh.width),
                    usize::from(seqh.height),
                    packet.pts,
                );
                Ok(Frame::Video(video))
            }
            _ => Err(Error::unsupported(
                "oxideav-svq: SVQ3 P/B macroblock-layer decode not yet implemented — see crates/oxideav-svq/README.md",
            )),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.pending = None;
        self.last_slice_header = None;
        self.eof = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::Svq1PictureType;
    use oxideav_core::TimeBase;

    /// Build Yuv420P video parameters for the encoder factory.
    fn encoder_params(width: u32, height: u32) -> CodecParameters {
        let mut params = CodecParameters::video(CodecId::new(crate::CODEC_ID_STR));
        params.width = Some(width);
        params.height = Some(height);
        params
    }

    /// Build a synthetic Yuv420P frame with deterministic content.
    fn synthetic_frame(width: usize, height: usize, seed: u32, pts: i64) -> Frame {
        let plane = |w: usize, h: usize, s: u32| -> oxideav_core::VideoPlane {
            oxideav_core::VideoPlane {
                stride: w,
                data: (0..w * h)
                    .map(|i| {
                        let x = (i % w) as u32;
                        let y = (i / w) as u32;
                        ((x * 3 + y * 5 + s) % 256) as u8
                    })
                    .collect(),
            }
        };
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        Frame::Video(oxideav_core::VideoFrame {
            pts: Some(pts),
            planes: vec![
                plane(width, height, seed),
                plane(cw, ch, seed + 60),
                plane(cw, ch, seed + 120),
            ],
        })
    }

    /// `make_encoder` → `make_decoder` round trip: an I + 2P sequence
    /// encodes through the registry Encoder trait and decodes through
    /// the registry Decoder trait; frame 0 is a keyframe, P-frames
    /// are not; every decoded frame carries the right geometry.
    #[test]
    fn registry_encoder_round_trips_through_registry_decoder() {
        let (width, height) = (64usize, 48usize);
        let params = encoder_params(width as u32, height as u32);
        let mut encoder = make_encoder(&params).expect("encoder builds");
        assert_eq!(encoder.codec_id().as_str(), "svq1");
        assert_eq!(encoder.output_params().width, Some(width as u32));

        let mut decoder = make_decoder(&params).expect("decoder builds");
        for (i, seed) in [7u32, 9, 11].into_iter().enumerate() {
            encoder
                .send_frame(&synthetic_frame(width, height, seed, i as i64))
                .expect("frame accepted");
            let packet = encoder.receive_packet().expect("packet produced");
            assert_eq!(packet.flags.keyframe, i == 0, "frame {i} keyframe flag");
            assert_eq!(packet.pts, Some(i as i64));

            decoder.send_packet(&packet).expect("packet accepted");
            let Frame::Video(video) = decoder.receive_frame().expect("frame decodes") else {
                panic!("expected a video frame");
            };
            assert_eq!(video.planes[0].stride, width, "frame {i} luma stride");
            assert_eq!(
                video.planes[0].data.len(),
                width * height,
                "frame {i} luma size"
            );
            assert_eq!(video.pts, Some(i as i64));
        }
        encoder.flush().expect("flush ok");
        assert!(matches!(encoder.receive_packet(), Err(Error::Eof)));
    }

    /// Rate controller: every emitted packet fits the per-frame byte
    /// budget, and the stream still round-trips through the registry
    /// decoder (λ scheduling is encoder policy — the wire stays
    /// conformant at any λ).
    #[test]
    fn rate_controller_meets_per_frame_budget() {
        let (width, height) = (64usize, 48usize);
        let params = encoder_params(width as u32, height as u32);
        let target = 700usize;
        let mut encoder = make_encoder_handle(&params).expect("encoder builds");
        encoder.set_target_frame_bytes(Some(target));
        let mut decoder = make_decoder(&params).expect("decoder builds");

        for (i, seed) in [7u32, 9, 11, 13].into_iter().enumerate() {
            encoder
                .send_frame(&synthetic_frame(width, height, seed, i as i64))
                .expect("frame accepted");
            let packet = encoder.receive_packet().expect("packet produced");
            assert!(
                packet.data.len() <= target,
                "frame {i}: {} bytes exceeds the {target}-byte budget",
                packet.data.len()
            );
            decoder.send_packet(&packet).expect("packet accepted");
            let Frame::Video(video) = decoder.receive_frame().expect("frame decodes") else {
                panic!("expected a video frame");
            };
            assert_eq!(video.planes[0].data.len(), width * height);
        }
    }

    /// Rate controller under a generous budget converges on the λ = 0
    /// (maximum-fidelity) encode — byte-identical to a fixed-λ = 0
    /// encoder over the same frames (the encoder is deterministic).
    #[test]
    fn rate_controller_chases_fidelity_under_generous_budget() {
        let (width, height) = (64usize, 48usize);
        let params = encoder_params(width as u32, height as u32);
        let mut rc = make_encoder_handle(&params).expect("encoder builds");
        rc.set_target_frame_bytes(Some(1 << 20));
        let mut fixed = make_encoder_handle(&params).expect("encoder builds");
        fixed.set_lambda(0);

        for (i, seed) in [7u32, 9, 11].into_iter().enumerate() {
            let frame = synthetic_frame(width, height, seed, i as i64);
            rc.send_frame(&frame).expect("rc frame accepted");
            fixed.send_frame(&frame).expect("fixed frame accepted");
            let rc_packet = rc.receive_packet().expect("rc packet");
            let fixed_packet = fixed.receive_packet().expect("fixed packet");
            assert_eq!(
                rc_packet.data, fixed_packet.data,
                "frame {i}: generous-budget rate control must match λ = 0"
            );
        }
    }

    /// An unachievable budget still emits a valid (best-effort,
    /// λ-ceiling) stream instead of failing — SVQ1 has a per-MB wire
    /// floor no λ can compress away.
    #[test]
    fn rate_controller_best_effort_on_unachievable_budget() {
        let (width, height) = (64usize, 48usize);
        let params = encoder_params(width as u32, height as u32);
        let mut encoder = make_encoder_handle(&params).expect("encoder builds");
        encoder.set_target_frame_bytes(Some(8));
        let mut decoder = make_decoder(&params).expect("decoder builds");

        encoder
            .send_frame(&synthetic_frame(width, height, 7, 0))
            .expect("frame accepted");
        let packet = encoder.receive_packet().expect("packet produced");
        assert!(packet.data.len() > 8, "the 8-byte budget is unachievable");
        decoder.send_packet(&packet).expect("packet accepted");
        assert!(matches!(
            decoder.receive_frame().expect("frame decodes"),
            Frame::Video(_)
        ));
    }

    /// Droppable cadence: `set_droppable_period(2)` produces
    /// `I B P B P`; the B packets carry picture type 2 and are
    /// reference-transparent — decoding the stream WITHOUT them
    /// yields byte-identical non-B frames (registry decoder level).
    #[test]
    fn droppable_period_emits_reference_transparent_b_frames() {
        let (width, height) = (64usize, 48usize);
        let params = encoder_params(width as u32, height as u32);
        let mut encoder = make_encoder_handle(&params).expect("encoder builds");
        encoder.set_droppable_period(2);

        let mut packets = Vec::new();
        for (i, seed) in [7u32, 9, 11, 13, 15].into_iter().enumerate() {
            encoder
                .send_frame(&synthetic_frame(width, height, seed, i as i64))
                .expect("frame accepted");
            packets.push(encoder.receive_packet().expect("packet produced"));
        }

        // Picture types on the wire: I B P B P.
        let picture_types: Vec<u8> = packets
            .iter()
            .map(|p| {
                crate::header::parse_frame_header(&p.data)
                    .expect("header parses")
                    .picture_type as u8
            })
            .collect();
        assert_eq!(picture_types, [0, 2, 1, 2, 1], "I B P B P cadence");

        // Full decode: every frame produces output.
        let decode = |selected: &[usize]| -> Vec<(usize, Vec<u8>)> {
            let mut decoder = make_decoder(&params).expect("decoder builds");
            let mut out = Vec::new();
            for &i in selected {
                decoder.send_packet(&packets[i]).expect("packet accepted");
                let Frame::Video(video) = decoder.receive_frame().expect("frame decodes") else {
                    panic!("expected a video frame");
                };
                out.push((i, video.planes[0].data.clone()));
            }
            out
        };
        let full = decode(&[0, 1, 2, 3, 4]);
        let dropped = decode(&[0, 2, 4]);

        // The non-B frames decode byte-identical with the B packets
        // discarded — droppable frames never entered the reference
        // chain on either side.
        for (i, luma) in &dropped {
            let (_, full_luma) = full.iter().find(|(j, _)| j == i).expect("frame present");
            assert_eq!(luma, full_luma, "frame {i} luma differs after B-drop");
        }
    }

    /// The registry encoder is registered under the `svq1` id.
    #[test]
    fn registers_svq1_encoder_id() {
        let mut ctx = RuntimeContext::new();
        super::register(&mut ctx);
        assert!(ctx.codecs.encoder_ids().any(|id| id.as_str() == "svq1"));
    }

    /// Bad geometry / non-video input is rejected.
    #[test]
    fn encoder_rejects_bad_input() {
        assert!(make_encoder(&encoder_params(0, 48)).is_err());
        assert!(make_encoder(&encoder_params(64, 4096)).is_err());
        let mut encoder = make_encoder(&encoder_params(64, 48)).expect("builds");
        // Undersized luma plane.
        let bad = Frame::Video(oxideav_core::VideoFrame {
            pts: None,
            planes: vec![
                oxideav_core::VideoPlane {
                    stride: 64,
                    data: vec![0u8; 64],
                },
                oxideav_core::VideoPlane {
                    stride: 32,
                    data: vec![0u8; 32 * 24],
                },
                oxideav_core::VideoPlane {
                    stride: 32,
                    data: vec![0u8; 32 * 24],
                },
            ],
        });
        assert!(encoder.send_frame(&bad).is_err());
    }

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
    fn decoder_decodes_intra_frame_end_to_end() {
        // The black-box conformance fixture (see
        // tests/svq1_intra_conformance.rs for the byte-exact
        // plane-level check) through the framework Decoder surface.
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut decoder = make_decoder(&params).expect("make_decoder ok");
        let pkt = make_packet(include_bytes!("../tests/fixtures/intra_176x144.svq1").to_vec());
        decoder.send_packet(&pkt).expect("send_packet ok");
        let frame = decoder.receive_frame().expect("intra frame decodes");
        let Frame::Video(video) = frame else {
            panic!("expected a video frame");
        };
        // Yuv420P bridge: Y at 176×144, chroma at 88×72.
        assert_eq!(video.planes.len(), 3);
        assert_eq!(video.planes[0].stride, 176);
        assert_eq!(video.planes[0].data.len(), 176 * 144);
        assert_eq!(video.planes[1].stride, 88);
        assert_eq!(video.planes[1].data.len(), 88 * 72);
        assert_eq!(video.planes[2].data.len(), 88 * 72);
        // Next receive without a packet → NeedMore.
        assert!(matches!(decoder.receive_frame(), Err(Error::NeedMore)));
    }

    #[test]
    fn decoder_decodes_p_frame_against_held_reference() {
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut decoder = make_decoder(&params).expect("make_decoder ok");
        let i_pkt = make_packet(include_bytes!("../tests/fixtures/intra_176x144.svq1").to_vec());
        decoder.send_packet(&i_pkt).expect("send I");
        decoder.receive_frame().expect("I decodes");
        let p_pkt = make_packet(include_bytes!("../tests/fixtures/inter_176x144_p.svq1").to_vec());
        decoder.send_packet(&p_pkt).expect("send P");
        let frame = decoder
            .receive_frame()
            .expect("P decodes against held reference");
        let Frame::Video(video) = frame else {
            panic!("expected a video frame");
        };
        assert_eq!(video.planes[0].data.len(), 176 * 144);

        // After reset the reference is dropped: the same P packet
        // must be rejected.
        decoder.reset().expect("reset ok");
        decoder.send_packet(&p_pkt).expect("send P again");
        let err = decoder
            .receive_frame()
            .expect_err("P without a reference must be rejected");
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn decoder_decodes_genuine_4mv_chain_end_to_end() {
        // The genuine-INTER_4MV chain (docs #197 inter-4mv, byte-
        // identical to enc_4mv_176x144_4f) through the framework
        // Decoder surface: exercises the multi-frame state machine +
        // the 4:1:0 → Yuv420P chroma bridge on real 4MV output, where
        // svq1_genuine_4mv_conformance.rs covers the native plane
        // decode. The framework luma plane is 1:1 with the native
        // decode, so it must match the black-box oracle Y exactly for
        // every frame; chroma is nearest-neighbour upscaled and only
        // its bridged geometry is checked here.
        const CHAIN: &[u8] = include_bytes!("../tests/fixtures/enc_4mv_176x144_4f.svq1");
        const ORACLE: &[u8] = include_bytes!("../tests/fixtures/enc_4mv_176x144_4f.yuv410p");
        const SIZES: [usize; 4] = [7601, 1357, 1178, 1076];
        const FRAME_YUV: usize = 176 * 144 + 2 * 44 * 36;

        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut decoder = make_decoder(&params).expect("make_decoder ok");
        let mut offset = 0usize;
        for (n, &size) in SIZES.iter().enumerate() {
            let pkt = make_packet(CHAIN[offset..offset + size].to_vec());
            offset += size;
            decoder.send_packet(&pkt).expect("send frame");
            let Frame::Video(video) = decoder.receive_frame().expect("frame decodes") else {
                panic!("expected a video frame for frame {n}");
            };
            // Yuv420P bridge geometry: Y 176×144, chroma 88×72.
            assert_eq!(video.planes.len(), 3, "frame {n} plane count");
            assert_eq!(video.planes[0].data.len(), 176 * 144, "frame {n} Y size");
            assert_eq!(video.planes[1].data.len(), 88 * 72, "frame {n} U size");
            assert_eq!(video.planes[2].data.len(), 88 * 72, "frame {n} V size");
            // Luma is 1:1 with the native decode → byte-exact vs the
            // independent oracle for every frame of the chain.
            let want_y = &ORACLE[n * FRAME_YUV..n * FRAME_YUV + 176 * 144];
            assert_eq!(video.planes[0].data, want_y, "frame {n} luma vs oracle");
        }
    }

    #[test]
    fn decoder_rejects_truncated_plane_data() {
        // A header-only packet parses at send_packet but runs out of
        // bits during the plane decode.
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut decoder = make_decoder(&params).expect("make_decoder ok");
        let pkt = make_packet(minimal_iframe_packet());
        decoder.send_packet(&pkt).expect("send_packet ok");
        let err = decoder
            .receive_frame()
            .expect_err("header-only packet has no plane data");
        assert!(matches!(err, Error::InvalidData(_)));
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
            reference: None,
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

    // ---- SVQ3 registry tests --------------------------------------

    /// SEQH extradata blob with frame-size-code 2 (176×144), all flags
    /// off, not protected. 12-bit payload → 2 bytes after rounding up.
    fn minimal_svq3_extradata() -> Vec<u8> {
        let payload = pack(&[(3, 2), (1, 0), (1, 0), (4, 0), (1, 0), (1, 0), (1, 0)]);
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&crate::svq3::SVQ3_SEQH_MAGIC);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload);
        out
    }

    /// Minimal SVQ3 wire-slice: v1 I-frame, size_size=1, num_mbs=99.
    fn minimal_svq3_slice_packet() -> Vec<u8> {
        let header_bits = pack(&[
            (3, 0b011), // ue(2) = I
            (1, 0),     // has more slices = 0
            (8, 7),     // frame number
            (5, 4),     // qp
            (1, 0),     // delta qp
            (1, 0),     // unknown
            (1, 0),     // optional loop stop
        ]);
        let prefix = (1 << 5) | 1; // size_size=1, version=1
        let mut wire = vec![prefix, header_bits.len() as u8];
        wire.extend_from_slice(&header_bits);
        wire
    }

    #[test]
    fn registers_svq3_codec_id() {
        let mut ctx = RuntimeContext::new();
        super::register(&mut ctx);
        assert!(ctx.codecs.decoder_ids().any(|id| id.as_str() == "svq3"));
    }

    #[test]
    fn registers_svq1_and_svq3_distinct_entries() {
        let mut ctx = RuntimeContext::new();
        super::register(&mut ctx);
        let ids: Vec<String> = ctx
            .codecs
            .decoder_ids()
            .map(|id| id.as_str().to_string())
            .collect();
        assert!(ids.iter().any(|id| id == "svq1"));
        assert!(ids.iter().any(|id| id == "svq3"));
    }

    #[test]
    fn svq3_probe_accepts_valid_slice_prefix() {
        let pkt = minimal_svq3_slice_packet();
        let tag = CodecTag::fourcc(b"SVQ3");
        let ctx = ProbeContext::new(&tag).packet(&pkt);
        assert_eq!(probe_svq3(&ctx), 1.0);
    }

    #[test]
    fn svq3_probe_rejects_unknown_version() {
        // version = 3 (only 1 and 2 are valid per the wiki spec)
        let buf = [(1 << 5) | 3u8];
        let tag = CodecTag::fourcc(b"SVQ3");
        let ctx = ProbeContext::new(&tag).packet(&buf);
        assert_eq!(probe_svq3(&ctx), 0.0);
    }

    #[test]
    fn svq3_probe_rejects_invalid_size_size() {
        // size_size = 0 (high 3 bits) — out of range. Version = 1 in
        // low 5 bits keeps the version check happy so the size-size
        // path is exercised.
        let buf = [1u8];
        let tag = CodecTag::fourcc(b"SVQ3");
        let ctx = ProbeContext::new(&tag).packet(&buf);
        assert_eq!(probe_svq3(&ctx), 0.0);
    }

    #[test]
    fn svq3_probe_partial_confidence_on_frame_end_byte() {
        let tag = CodecTag::fourcc(b"SVQ3");
        let ctx = ProbeContext::new(&tag).packet(&[crate::svq3::SVQ3_FRAME_END]);
        assert_eq!(probe_svq3(&ctx), 0.5);
    }

    #[test]
    fn svq3_probe_partial_confidence_without_packet() {
        let tag = CodecTag::fourcc(b"SVQ3");
        let ctx = ProbeContext::new(&tag);
        assert_eq!(probe_svq3(&ctx), 0.5);
    }

    #[test]
    fn svq3_probe_rejects_empty_packet() {
        let tag = CodecTag::fourcc(b"SVQ3");
        let ctx = ProbeContext::new(&tag).packet(&[]);
        assert_eq!(probe_svq3(&ctx), 0.0);
    }

    #[test]
    fn svq3_decoder_parses_extradata_at_construction() {
        let mut params = CodecParameters::video(CodecId::new(SVQ3_CODEC_ID_STR));
        params.extradata = minimal_svq3_extradata();
        let dec = make_svq3_decoder(&params).expect("make_svq3_decoder ok");
        // We need to downcast back to the concrete handle to inspect
        // the sequence header — build directly instead so the
        // accessor is exercised.
        let _boxed: Box<dyn Decoder> = dec;
        // Direct-construct to exercise sequence_header().
        let mut params2 = CodecParameters::video(CodecId::new(SVQ3_CODEC_ID_STR));
        params2.extradata = minimal_svq3_extradata();
        let handle = Svq3DecoderHandle {
            codec_id: CodecId::new(SVQ3_CODEC_ID_STR),
            sequence_header: crate::svq3::parse_extradata(&params2.extradata).ok(),
            pending: None,
            last_slice_header: None,
            eof: false,
        };
        let seqh = handle.sequence_header().expect("seqh parsed");
        assert_eq!(seqh.width, 176);
        assert_eq!(seqh.height, 144);
    }

    #[test]
    fn svq3_decoder_send_then_receive_without_seqh_errors() {
        // No extradata was supplied, so the intra frame walk cannot
        // derive the macroblock grid — receive_frame reports the
        // missing SEQH as InvalidData.
        let params = CodecParameters::video(CodecId::new(SVQ3_CODEC_ID_STR));
        let mut decoder = make_svq3_decoder(&params).expect("make_svq3_decoder ok");
        let pkt = make_packet(minimal_svq3_slice_packet());
        decoder.send_packet(&pkt).expect("send_packet ok");
        let err = decoder
            .receive_frame()
            .expect_err("intra decode requires the SEQH grid");
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn svq3_decoder_records_slice_header() {
        let mut handle = Svq3DecoderHandle {
            codec_id: CodecId::new(SVQ3_CODEC_ID_STR),
            sequence_header: None,
            pending: None,
            last_slice_header: None,
            eof: false,
        };
        let pkt = make_packet(minimal_svq3_slice_packet());
        handle.send_packet(&pkt).expect("send_packet ok");
        let hdr = handle.last_slice_header().expect("slice header recorded");
        assert_eq!(hdr.frame_type, crate::svq3::Svq3FrameType::Intra);
        assert_eq!(hdr.frame_number, 7);
        assert_eq!(hdr.slice_qp, 4);
    }

    #[test]
    fn svq3_decoder_accepts_frame_end_packet() {
        let params = CodecParameters::video(CodecId::new(SVQ3_CODEC_ID_STR));
        let mut decoder = make_svq3_decoder(&params).expect("make_svq3_decoder ok");
        // A 0xFF-leading packet is the frame-data-end sentinel — the
        // decoder accepts it (no header parsed) and receive_frame
        // reports that no picture was carried.
        let pkt = make_packet(vec![crate::svq3::SVQ3_FRAME_END]);
        decoder
            .send_packet(&pkt)
            .expect("send_packet ok on frame-end byte");
        let err = decoder
            .receive_frame()
            .expect_err("a frame-end sentinel carries no picture");
        assert!(matches!(err, Error::NeedMore));
    }

    /// SMI-wrapped SVQ3 extradata for an explicit-dimension 32×32
    /// stream, shaped like a QuickTime visual sample entry trailer
    /// (4-byte wrapper size + `SMI ` + `SEQH` + 4-byte length +
    /// payload), exercising the flexible SEQH-marker location.
    fn svq3_extradata_32x32_smi_wrapped() -> Vec<u8> {
        let payload = pack(&[
            (3, 7),   // frame_size_code 7 → explicit dimensions
            (12, 32), // width
            (12, 32), // height
            (1, 0),   // halfpel
            (1, 0),   // thirdpel
            (4, 0),   // unknown
            (1, 1),   // no B-frames
            (1, 0),   // optional-data loop stop
            (1, 0),   // not protected
        ]);
        let mut out = Vec::new();
        out.extend_from_slice(&((payload.len() as u32) + 16).to_be_bytes());
        out.extend_from_slice(b"SMI ");
        out.extend_from_slice(&crate::svq3::SVQ3_SEQH_MAGIC);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&[0, 0, 0, 0]); // zero filler
        out
    }

    /// A synthetic 32×32 intra access unit: version-1 slice, quantiser
    /// 13, four all-empty intra-4×4 macroblocks (the 14-bit unit the
    /// staged fixture census pinned), frame-end sentinel.
    fn svq3_intra_au_32x32_flat() -> Vec<u8> {
        let mut items: Vec<(u32, u32)> = vec![
            (3, 0b011), // ue(2) = I frame code
            (1, 0),     // v1: no more slices
            (8, 0),     // frame number
            (5, 13),    // slice quantiser
            (1, 0),     // delta qp flag
            (1, 0),     // unknown
            (1, 0),     // optional-data loop stop
        ];
        for _ in 0..4 {
            // type ue(0) + eight pair codes ue(0) + CBP ue(3) = "00001".
            for _ in 0..9 {
                items.push((1, 1));
            }
            items.push((5, 1));
        }
        let body = pack(&items);
        let prefix = (1u8 << 5) | 1; // slice_size_size = 1, version 1
        let mut au = vec![prefix, body.len() as u8];
        au.extend_from_slice(&body);
        au.push(crate::svq3::SVQ3_FRAME_END);
        au
    }

    #[test]
    fn svq3_intra_end_to_end_decodes_flat_frame() {
        let mut params = CodecParameters::video(CodecId::new(SVQ3_CODEC_ID_STR));
        params.extradata = svq3_extradata_32x32_smi_wrapped();
        let mut decoder = make_svq3_decoder(&params).expect("make_svq3_decoder ok");
        decoder
            .send_packet(&make_packet(svq3_intra_au_32x32_flat()))
            .expect("send_packet ok");
        let Frame::Video(video) = decoder.receive_frame().expect("intra AU decodes") else {
            panic!("expected a video frame");
        };
        assert_eq!(video.planes.len(), 3);
        assert_eq!(video.planes[0].stride, 32);
        assert_eq!(video.planes[0].data.len(), 32 * 32);
        assert_eq!(video.planes[1].data.len(), 16 * 16);
        assert_eq!(video.planes[2].data.len(), 16 * 16);
        // All-empty intra-4×4 macroblocks: the DC prediction chain
        // stays at the 128 fallback on every plane.
        assert!(video
            .planes
            .iter()
            .all(|p| p.data.iter().all(|&s| s == 128)));
    }

    #[test]
    fn svq3_decoder_rejects_malformed_packet() {
        let params = CodecParameters::video(CodecId::new(SVQ3_CODEC_ID_STR));
        let mut decoder = make_svq3_decoder(&params).expect("make_svq3_decoder ok");
        // Unknown version 3
        let prefix = (1 << 5) | 3;
        let err = decoder
            .send_packet(&make_packet(vec![prefix, 0x01, 0x00]))
            .expect_err("unknown version must be rejected at send_packet");
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn svq3_decoder_send_twice_without_drain_errors() {
        let params = CodecParameters::video(CodecId::new(SVQ3_CODEC_ID_STR));
        let mut decoder = make_svq3_decoder(&params).expect("make_svq3_decoder ok");
        let pkt = make_packet(minimal_svq3_slice_packet());
        decoder.send_packet(&pkt).expect("first send_packet ok");
        let err = decoder
            .send_packet(&pkt)
            .expect_err("second send_packet without receive_frame must reject");
        assert!(matches!(err, Error::Other(_)));
    }

    #[test]
    fn svq3_decoder_flush_then_receive_signals_eof() {
        let params = CodecParameters::video(CodecId::new(SVQ3_CODEC_ID_STR));
        let mut decoder = make_svq3_decoder(&params).expect("make_svq3_decoder ok");
        decoder.flush().expect("flush ok");
        let err = decoder
            .receive_frame()
            .expect_err("post-flush drain — must signal Eof");
        assert!(matches!(err, Error::Eof));
    }

    #[test]
    fn svq3_decoder_reset_clears_state() {
        let mut handle = Svq3DecoderHandle {
            codec_id: CodecId::new(SVQ3_CODEC_ID_STR),
            sequence_header: None,
            pending: None,
            last_slice_header: None,
            eof: true,
        };
        let pkt = make_packet(minimal_svq3_slice_packet());
        handle.send_packet(&pkt).expect("send_packet ok");
        assert!(handle.last_slice_header().is_some());
        assert!(handle.pending.is_some());
        handle.reset().expect("reset ok");
        assert!(handle.last_slice_header().is_none());
        assert!(handle.pending.is_none());
        assert!(!handle.eof);
    }
}
