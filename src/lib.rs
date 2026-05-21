//! Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec.
//!
//! **Round 2 — `oxideav-core` framework integration.**
//!
//! Round 1 landed the structural SVQ1 frame-header parser. Round 2
//! wires that parser into the framework registry via the default-on
//! `registry` cargo feature: the crate now installs a SVQ1 codec
//! entry in [`oxideav_core::CodecRegistry`] under the FourCC tags
//! enumerated by `docs/video/svq1/wiki/Sorenson_Video_1.wiki` line 9
//! (`svq1` / `SVQ1` / `svqi`) and exposes a [`oxideav_core::Decoder`]
//! implementation whose `send_packet` parses the frame header and
//! whose `receive_frame` returns
//! [`oxideav_core::Error::Unsupported`] for the actual pixel-data
//! decode — the SVQ1 multi-stage VQ codebooks + per-level VLC tables
//! of the spec's "Appendix A: SVQ1 Data Tables" are still blocked on
//! the docs-collaborator extraction task (confirmed at round-2
//! dispatch).
//!
//! ## Standalone vs registry-integrated
//!
//! The default-on `registry` cargo feature pulls in `oxideav-core` and
//! installs the framework `Decoder` trait implementation plus
//! `register_codecs(reg)` / `register(ctx)` entry points. With the
//! feature off (`default-features = false`), the crate exposes a
//! minimal `oxideav-core`-free API: [`parse_frame_header`], the
//! [`Svq1FrameHeader`] result, the [`BitReader`] primitive, and the
//! [`Error`] enum.
//!
//! ## What's available at each layer
//!
//! * Standalone (`--no-default-features`):
//!     * [`parse_frame_header`] — walks every bit of the SVQ1 chunk
//!       header documented in
//!       `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format
//!       And Header" and returns a typed [`Svq1FrameHeader`].
//!     * [`FRAME_SIZE_TABLE`] — the seven well-known dimensions
//!       (160×120 / 128×96 / 176×144 / 352×288 / 704×576 / 240×180 /
//!       320×240).
//!     * [`SVQ1_FOURCC_CODES`] — `svq1`, `SVQ1`, `svqi` (line 9 of the
//!       wiki spec).
//!     * [`Error`] structural error variants plus the
//!       [`Error::NotImplemented`] sentinel for not-yet-wired API
//!       surfaces.
//! * Registry-integrated (default features):
//!     * Everything above, plus:
//!     * `register(&mut RuntimeContext)` — install the SVQ1 codec into
//!       a runtime.
//!     * `register_codecs(&mut CodecRegistry)` — same thing against a
//!       registry directly.
//!     * `probe_svq1(&ProbeContext)` — disambiguating probe wired into
//!       the registration's tag set.
//!     * `make_decoder` factory + `Svq1DecoderHandle` exposing
//!       [`Svq1DecoderHandle::last_header`] for the parsed header.
//!     * `From<crate::Error> for oxideav_core::Error` conversion.
//!
//! ## What round 2 does **not** deliver
//!
//! * The encoded plane data (`Y`, `U`, `V` planes) is **not** decoded.
//!   `receive_frame` returns `Error::Unsupported` until the codebook
//!   docs-gap closes.
//! * The embedded-string body is captured raw; de-obfuscation is
//!   deferred until the per-stream XOR table is pinned in `docs/`.
//! * The checksum byte is captured but not verified — the wiki spec
//!   itself notes "The specific details of the checksum coding are
//!   not all known".
//! * No SVQ3 work yet. SVQ3 is documented in `docs/video/svq3/wiki/`
//!   but the round-2 prompt scoped to SVQ1.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod bitreader;
mod error;
mod header;

#[cfg(feature = "registry")]
mod registry;

pub use crate::bitreader::BitReader;
pub use crate::error::{Error, Result};
pub use crate::header::{
    parse_frame_header, ChecksumTrailer, EmbeddedString, Svq1FrameHeader, Svq1PictureType,
    FRAME_SIZE_TABLE,
};

#[cfg(feature = "registry")]
pub use crate::registry::{
    __oxideav_entry, make_decoder, probe_svq1, register, register_codecs, Svq1DecoderHandle,
};

/// Stable codec id used in the framework registry.
pub const CODEC_ID_STR: &str = "svq1";

/// FourCC codes the wiki spec attaches to SVQ1 in QuickTime / AVI
/// containers (`docs/video/svq1/wiki/Sorenson_Video_1.wiki` line 9 —
/// "FOURCCs: svq1, SVQ1, svqi"). Listed in the order the wiki page
/// enumerates them. Both `svq1` and `SVQ1` upper-case to the same
/// `CodecTag::fourcc` value; `svqi` is its own tag.
pub const SVQ1_FOURCC_CODES: &[&[u8; 4]] = &[b"svq1", b"SVQ1", b"svqi"];
