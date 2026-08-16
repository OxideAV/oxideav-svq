//! Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec.
//!
//! Implemented clean-room from the specifications staged under
//! `docs/video/svq1/` and `docs/video/svq3/` (see the crate README for
//! the per-codec state and the CHANGELOG for round-by-round history).
//!
//! ## Module map
//!
//! **SVQ1** — decoder (byte-exact against two independent black-box
//! oracle families) + full I/P/B encoder:
//!
//! * [`svq1_vlc`] — the sixteen wire VLC tables as verified
//!   prefix-code decoders.
//! * [`svq1_codebook`] — the canonical 23 040-byte codebook region,
//!   page layout + hierarchical vector tile order.
//! * [`svq1_plane`] — whole-frame decode: per-plane MB raster scan,
//!   breadth-first block-tree walk, stage accumulation, the inter
//!   path (modes, MVs, half-pel MC) and [`svq1_plane::decode_frame`].
//! * [`svq1_enc`] / [`svq1_enc_tree`] / [`svq1_enc_leaf`] /
//!   [`svq1_enc_inter`] — the adaptive λ-tree I/P/B encoder.
//! * Support layers: [`svq1_blocktree`], [`svq1_mc`], [`svq1_mean`],
//!   [`svq1_motion_predictor`], [`svq1_mv_cache`],
//!   [`svq1_reconstruct`], [`svq1_stage_indices`],
//!   [`svq1_helper_luts`].
//!
//! **SVQ3** — parse + reconstruction layers driven from the staged
//! spec chapters (`docs/video/svq3/spec/01`–`06`):
//!
//! * [`svq3`] — `SEQH` extradata, slice envelope/header parse, the
//!   universal variable-length code (spec/06 §1), MB grid geometry.
//! * [`svq3_mb`] — MB-type classification, intra-4×4 prediction-mode
//!   VLC + context resolution, MV-precision selector.
//! * [`svq3_mv`] — signed universal-code reads: MV differences +
//!   quantiser delta.
//! * [`svq3_coeff`] — the three residual (level, run) code books +
//!   escape constructions (tables/05–06) and the per-block residual
//!   decoders.
//! * [`svq3_scan`] — the two 4×4 coefficient scan orders + placement
//!   helpers (tables/04).
//! * [`svq3_dequant`] — the core 4×4 inverse transform basis, the
//!   dequant ladder + chroma quantiser remap, both secondary
//!   transforms (spec/04), and the fused residual interleave.
//! * [`svq3_pred`] — the intra predictors (4×4 modes, 16×16
//!   plane/DC, chroma DC) and the `Clip1` writeback.
//! * [`svq3_recon`] — per-macroblock reconstruction composition for
//!   the 4×4-intra and 16×16-intra regimes + chroma planes.
//! * [`svq3_mc`] — reference-plane views and the third-pel /
//!   half-pel interpolation kernels (spec/05).
//! * [`svq3_picture`] — the picture canvas, cross-MB neighbour
//!   binding, and the `VideoFrame` output bridges.
//! * [`svq3_frame`] — the slice-level intra access-unit decoder
//!   (frame walk) composing all of the above.
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
//! ## Known SVQ1 tails (not blocking decode)
//!
//! * The embedded-string body is captured raw; de-obfuscation is
//!   deferred until the per-stream XOR table is pinned in `docs/`.
//! * The checksum byte is captured but not verified — the wiki spec
//!   itself notes "The specific details of the checksum coding are
//!   not all known".
//! * INTER_4MV wire coverage: the mode is minted by OUR encoder
//!   (`tests/svq1_enc_inter_conformance.rs`) because no available
//!   reference ENCODER binary emits it, but its DECODE is now
//!   confirmed by an INDEPENDENT, separately-written black-box decode
//!   oracle — `docs/video/svq1/fixtures/inter-4mv/` (#197) ships that
//!   oracle's byte-exact reconstruction plus an independent wire mode
//!   census, both reproduced here
//!   (`tests/svq1_genuine_4mv_conformance.rs`): every P-frame's luma
//!   grid decodes fully INTER_4MV, 348 INTER_4MV macroblocks in all.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod bitreader;
mod error;
mod header;
// The modules below are internal — exposed only so the crate's own
// tests / benches / fuzz harness can reach the staged decode+encode
// layers directly. They are NOT part of the stable public API, so they
// are `#[doc(hidden)]` to keep them out of the rendered docs and out of
// cargo-semver-checks' public-API surface. `svq3` is the one exception:
// it *defines* the stable `Svq3SequenceHeader` / `Svq3SliceHeader`
// types the registry `Svq3DecoderHandle` accessors return, so it stays
// visible and hides its internal items individually.
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_blocktree;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_codebook;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_enc;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_enc_inter;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_enc_leaf;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_enc_tree;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_helper_luts;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_mc;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_mean;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_motion_predictor;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_mv_cache;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_plane;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_reconstruct;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_stage_indices;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq1_vlc;
// Mixed module — defines the stable SVQ3 header types (kept visible) plus
// internal helpers (hidden individually inside the module).
pub mod svq3;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_cbp;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_coeff;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_dequant;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_frame;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_mb;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_mc;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_mv;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_picture;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_pred;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_recon;
#[doc(hidden)] // internal — exposed for tests/fuzz; not part of the stable API
pub mod svq3_scan;

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
    __oxideav_entry, make_decoder, make_encoder, make_svq3_decoder, probe_svq1, probe_svq3,
    register, register_codecs, Svq1DecoderHandle, Svq1EncoderHandle, Svq3DecoderHandle,
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
