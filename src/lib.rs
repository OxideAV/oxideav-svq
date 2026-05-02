//! Pure-Rust **Sorenson Video** family — a single crate covering every
//! Sorenson Media codec we ship. Module per version:
//!
//! * [`v1`] — Sorenson Video 1 (FourCC `SVQ1`), late-1990s QuickTime VQ.
//! * `v3` (planned) — Sorenson Video 3 (FourCC `SVQ3`), an H.264-class
//!   codec; trace doc lives at `docs/video/svq3/svq3-trace-reverse-engineering.md`.
//!
//! Today the crate registers `CodecId("svq1")` only. Future rounds add
//! `CodecId("svq3")` against the same registry call.
//!
//! See `crates/oxideav-svq/src/v1/lib.rs` doc comment (or the per-module
//! sources) for SVQ1 specifics.

#![allow(clippy::needless_range_loop)]
#![allow(clippy::too_many_arguments)]

pub mod v1;

use oxideav_core::CodecRegistry;

/// Register every Sorenson Video variant this crate supports against a
/// codec registry. Round 1 only registers SVQ1; round 2+ extends to
/// SVQ3 once `v3::` lands.
pub fn register(reg: &mut CodecRegistry) {
    v1::register(reg);
}

/// Re-export of [`v1::CODEC_ID_STR`] for backwards compatibility with
/// callers that previously imported `oxideav_svq::CODEC_ID_STR`.
pub use v1::CODEC_ID_STR;
