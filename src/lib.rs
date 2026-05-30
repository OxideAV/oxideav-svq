//! Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec.
//!
//! **Round 8 — SVQ1 block-tree subdivision walker.**
//!
//! Round 8 lands the recursive subdivide-vs-quantise decision tree
//! the SVQ1 wire format defines in
//! `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Decoding Intraframe
//! Plane Data" as a typed module ([`svq1_blocktree`]). The walker
//! covers the spec's six levels (L=5 16×16 down to L=0 4×2), reads
//! one bit per non-leaf decision (with L=0 short-circuiting to
//! "quantise"), and surfaces the wiki spec's
//! `(stages > 0) && (level >= 4)` error path through a new
//! [`Error::InvalidLevelQuantise`] variant. The L=4 / L=5 rejection
//! is corroborated by the clean-room codebook-extraction docs
//! `docs/video/svq1/spec/14.10-codebook-L4.md` and
//! `docs/video/svq1/spec/14.11-codebook-L5.md` (RESOLVED 2026-05-30
//! — no codebook stored at those levels in the Sorenson Video TM
//! for QT R2.0 build; the codebook region only covers L=0..L=3).
//! The per-leaf payload (stages-count VLC + mean VLC + codebook
//! selector bits) is still gated on the L=0..L=3 codebook layout
//! work tracked in `docs/video/svq1/CODEBOOK_GAP.md`.
//!
//! ## Earlier rounds (carried forward)
//!
//! Round 5 — SVQ3 residual coefficient walker.
//!
//! Round 4 landed the SVQ3 macroblock-type tree walk + the intra-
//! prediction lookup tables. Round 5 adds the per-block residual
//! coefficient walker described in
//! `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient
//! decoding": the three coefficient-table variants the wiki spec
//! enumerates (2×2 chroma DC, alternative-scan 4×4 luma-intra-with-
//! low-quantiser, normal-zigzag everything else) plus the two
//! run-correction arrays (`intra_run`, `inter_run`). The block-level
//! walkers ([`svq3_coeff::read_chroma_dc_block`],
//! [`svq3_coeff::read_alt_scan_half`],
//! [`svq3_coeff::read_normal_scan_block`]) loop until either the
//! end-of-block sentinel (`code = 0`) is seen or the block's
//! coefficient capacity is reached; structural overflow returns
//! [`Error::BadBitWidth`].
//!
//! ## Earlier rounds (carried forward)
//!
//! Round 4 added the per-macroblock type-Golomb walk + classification
//! per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
//! layer", plus the two fixed tables documented in §"Intra macroblock
//! information decoding" (the 25-entry intra-mode pair table and the
//! 6×6×5 intra-mode context-lookup table) and the 4×4 intra scan
//! order. The macroblock walker [`svq3_mb::read_mb_type`] decodes a
//! single `ue(v)` exp-Golomb code, classifies it against the
//! enclosing slice's frame type, and returns a typed
//! [`svq3_mb::Svq3MbType`]. CBP / motion-vector / intra-mode-pair
//! Golomb / intra-prediction wiring remain out of scope — those
//! sub-streams either depend on the H.264 CBP table (cross-spec
//! back-reference) or on the SVQ3 MV component VLC, neither of which
//! is enumerated bit-for-bit in the local mirror.
//!
//! ## Earlier rounds (carried forward)
//!
//! Round 3 — SVQ3 SEQH + slice-header parser, structural only. Round
//! 2 wired SVQ1 into the framework registry, gated behind the
//! default-on `registry` cargo feature.
//!
//! Round 2 — `oxideav-core` framework integration.
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
pub mod svq1_blocktree;
pub mod svq3;
pub mod svq3_coeff;
pub mod svq3_mb;

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
    __oxideav_entry, make_decoder, make_svq3_decoder, probe_svq1, probe_svq3, register,
    register_codecs, Svq1DecoderHandle, Svq3DecoderHandle,
};

/// Stable codec id used in the framework registry for SVQ1.
pub const CODEC_ID_STR: &str = "svq1";

/// Stable codec id used in the framework registry for SVQ3.
pub const SVQ3_CODEC_ID_STR: &str = "svq3";

/// FourCC codes the wiki spec attaches to SVQ1 in QuickTime / AVI
/// containers (`docs/video/svq1/wiki/Sorenson_Video_1.wiki` line 9 —
/// "FOURCCs: svq1, SVQ1, svqi"). Listed in the order the wiki page
/// enumerates them. Both `svq1` and `SVQ1` upper-case to the same
/// `CodecTag::fourcc` value; `svqi` is its own tag.
pub const SVQ1_FOURCC_CODES: &[&[u8; 4]] = &[b"svq1", b"SVQ1", b"svqi"];

/// FourCC codes the wiki spec attaches to SVQ3 (`docs/video/svq3/
/// wiki/Sorenson_Video_3.wiki` line 7 — "FOURCCs: SVQ3"). Listed in
/// the order the wiki page enumerates them.
pub const SVQ3_FOURCC_CODES: &[&[u8; 4]] = &[b"SVQ3"];
