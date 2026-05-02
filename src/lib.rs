//! Pure-Rust **Sorenson Video 1 (SVQ1)** decoder.
//!
//! SVQ1 (FourCC `SVQ1`) is the late-1990s Apple QuickTime VQ codec
//! described conceptually in
//! `docs/video/svq1/svq1-trace-reverse-engineering.md`. The bitstream
//! is MSB-first big-endian and structured as:
//!
//! * 22-bit `frame_code` packet prefix (`{0x20, 0x40, 0x50, 0x60}`).
//! * Frame header: 8-bit `temporal_reference`, 2-bit `frame_type`
//!   (0 = I, 1 = P, 2 = P-non-ref). I-frames carry an extra 8 bits
//!   of reserved + a 3-bit `frame_size_code` (0..6 picks one of seven
//!   preset sizes, 7 means an explicit `u(12)+u(12)` width/height).
//! * Optional checksum (1 bit + 4 bits if set) and optional extra-data
//!   block (1 bit + 8 reserved + a stop-1+u(8) chain).
//! * Body: three planes Y, U, V at YUV 4:1:0 sampling, each a raster
//!   of 16x16 macroblocks over the 16-aligned plane size, decoded by
//!   a per-leaf hierarchical multistage VQ:
//!   * 1-bit split flag at each level *L > 0*.
//!   * `multistage_count` VLC (-1 .. +6, level-dependent).
//!   * `mean` VLC (8-bit unsigned for INTRA, 9-bit signed for INTER).
//!   * `count` four-bit codebook indices, one per stage.
//!
//! Output is `PixelFormat::Yuv420P` — the native 4:1:0 planes are
//! upsampled 2x in each chroma dimension at the decoder boundary, since
//! oxideav-core does not expose a 4:1:0 variant.
//!
//! # Scope
//!
//! Round-1 decoder: parses every byte of a "plain" (`frame_code = 0x20`)
//! I-frame from a real QuickTime-muxed SVQ1 stream and emits a
//! `VideoFrame`. P-frame motion-compensation, the byte-swap header
//! obfuscation pre-pass, the embedded-checksum and embedded-string
//! variants, and the multistage VLC / fixed codebook tables themselves
//! are stubbed — see `README.md`'s **Gaps** section. Today's decoder
//! falls back to **mean-only flat-fill** at every leaf when no codebook
//! is wired up, which is enough to validate the full bitstream walk
//! end-to-end against ffmpeg-encoded fixtures and to land a real PSNR
//! number.
//!
//! # Quick use
//!
//! ```no_run
//! use oxideav_core::{CodecId, Packet, TimeBase, Decoder, Frame};
//! use oxideav_svq1::decoder::Svq1Decoder;
//!
//! let packet_bytes: Vec<u8> = vec![];
//! let mut dec = Svq1Decoder::new(CodecId::new(oxideav_svq1::CODEC_ID_STR));
//! let pkt = oxideav_core::Packet::new(0, oxideav_core::TimeBase::new(1, 30), packet_bytes);
//! dec.send_packet(&pkt)?;
//! match dec.receive_frame()? {
//!     oxideav_core::Frame::Video(_vf) => { /* vf planes are Yuv420P */ }
//!     _ => unreachable!(),
//! }
//! # Ok::<(), oxideav_core::Error>(())
//! ```

#![allow(clippy::needless_range_loop)]
#![allow(clippy::too_many_arguments)]

pub mod codebook;
pub mod decoder;
pub mod header;
pub mod vq;

use oxideav_core::{CodecCapabilities, CodecId, CodecTag};
use oxideav_core::{CodecInfo, CodecRegistry};

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
