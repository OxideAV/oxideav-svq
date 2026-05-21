//! Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec.
//!
//! **Round 1 — structural frame-header parse.**
//!
//! The crate's master branch was orphan-rebuilt after the 2026-05-06
//! docs audit retired the previous trace document. Round 1 lands the
//! foundational SVQ1 frame-header parser documented in
//! `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
//! Header" — that wiki page is a verbatim local mirror of the
//! multimedia.cx Sorenson_Video_1 wiki (fetched 2026-05-06, CC-BY-SA
//! per multimedia.cx terms) and is the only authoritative source
//! consulted during round-1 implementation.
//!
//! ## What round 1 delivers
//!
//! * A typed [`Svq1FrameHeader`] return value populated with the
//!   structurally-decoded fields the wiki spec enumerates: the 22-bit
//!   frame code, the 8-bit temporal reference, the 2-bit picture-type
//!   (decoded to a [`Svq1PictureType`] enum), the optional I-frame
//!   checksum / embedded-string / unknown-prefix / frame-size code,
//!   the explicit width/height pair when the frame-size code escapes,
//!   the optional structural checksum trailer, and the optional
//!   `unknown_flag_1` trailer plus its byte-loop tail.
//! * A frame-size-code lookup table ([`FRAME_SIZE_TABLE`]) covering
//!   the seven standard dimensions enumerated by the spec
//!   (160×120 / 128×96 / 176×144 / 352×288 / 704×576 / 240×180 /
//!   320×240) plus the escape-to-explicit-12+12 branch.
//! * Four documented structural error conditions: invalid frame code
//!   (`frame_code & 0x60 == 0`), reserved picture-type value `3`,
//!   non-zero `unknown_field_1` in the checksum trailer, and
//!   premature end of input.
//! * An MSB-first [`BitReader`] sized for the header fields and the
//!   variable-length byte-loop tail.
//!
//! ## What round 1 does **not** deliver
//!
//! * The encoded plane data (`Y`, `U`, `V` planes) is **not** decoded.
//!   That requires the SVQ1 codebook tables and the per-level VLC
//!   tables enumerated in the wiki spec's "Appendix A: SVQ1 Data
//!   Tables" — both blocked on the existing docs-collaborator task
//!   tracking the codebook byte-list extraction (see the gap note
//!   embedded in this crate's `CHANGELOG.md`).
//! * The embedded-string body is captured raw; de-obfuscation is
//!   deferred until the per-stream XOR table is pinned in `docs/`.
//! * The checksum byte is captured but not verified — the wiki spec
//!   itself notes "The specific details of the checksum coding are
//!   not all known".
//! * No SVQ3 work yet. SVQ3 is documented in `docs/video/svq3/wiki/`
//!   but the round-1 prompt scoped to SVQ1's frame-header layer.
//! * No `oxideav-core` framework integration yet — round 1 is a
//!   self-contained structural parser. A subsequent round will add a
//!   default-on `registry` cargo feature that wires the parser into
//!   `CodecResolver` via the per-FourCC tag declaration.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod bitreader;
mod error;
mod header;

pub use crate::bitreader::BitReader;
pub use crate::error::{Error, Result};
pub use crate::header::{
    parse_frame_header, ChecksumTrailer, EmbeddedString, Svq1FrameHeader, Svq1PictureType,
    FRAME_SIZE_TABLE,
};

/// Stable codec id used in the framework registry once the
/// `oxideav-core` integration lands in a subsequent round.
pub const CODEC_ID_STR: &str = "svq1";

/// FourCC codes the wiki spec attaches to SVQ1 in QuickTime / AVI
/// containers (`docs/video/svq1/wiki/Sorenson_Video_1.wiki` §header
/// metadata). Listed in the order the wiki page enumerates them.
pub const SVQ1_FOURCC_CODES: &[&[u8; 4]] = &[b"svq1", b"SVQ1", b"svqi"];
