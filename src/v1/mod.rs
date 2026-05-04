//! Sorenson Video 1 (SVQ1).
//!
//! Late-1990s Apple QuickTime VQ codec described conceptually in
//! `docs/video/svq1/svq1-trace-reverse-engineering.md`. See the
//! per-module sources below for the bitstream-walk + VQ + plane-output
//! details:
//!
//! * [`header`] — frame-header parser (frame_code, frame_type, sizes,
//!   checksum, extra-data block).
//! * [`codebook`] — sub-block geometry + level-stage caps. The actual
//!   multistage VLC tables and 6×6×16 codebook bytes are stubbed
//!   pending a clean-room transcription (the trace doc doesn't cover
//!   them).
//! * [`vq`] — plane padding, MB-raster iterator, 4:1:0 → 4:2:0 chroma
//!   upsample, flat-fill body decoder.
//! * [`decoder`] — Decoder trait wiring + prev-frame dim cache.
//!
//! # Scope
//!
//! Round-1 decoder: parses every byte of a "plain" (`frame_code = 0x20`)
//! I-frame from a real QuickTime-muxed SVQ1 stream and emits a
//! `VideoFrame`. P-frame motion-compensation, the byte-swap header
//! obfuscation pre-pass, the embedded-checksum / embedded-string
//! variants, and the multistage VLC + fixed codebook tables themselves
//! are stubbed. Today's decoder falls back to **mean-only flat-fill**
//! at every leaf when no codebook is wired up, which is enough to
//! validate the full bitstream walk end-to-end against ffmpeg-encoded
//! fixtures.

pub mod codebook;
pub mod decoder;
pub mod header;
pub mod tables;
pub mod vlc;
pub mod vq;

use oxideav_core::{CodecCapabilities, CodecId, CodecInfo, CodecRegistry, CodecTag};

/// The canonical oxideav codec id for Sorenson Video 1.
pub const CODEC_ID_STR: &str = "svq1";

/// Register the SVQ1 decoder with a codec registry.
///
/// Claims the QuickTime FourCC `SVQ1` (and the lowercase `svq1`
/// spelling sometimes seen in MOV `stsd` boxes).
pub fn register(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("svq1_sw")
        .with_lossy(true)
        .with_intra_only(false)
        .with_max_size(4096, 4096);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(decoder::make_decoder)
            .tags([CodecTag::fourcc(b"SVQ1"), CodecTag::fourcc(b"svq1")]),
    );
}
