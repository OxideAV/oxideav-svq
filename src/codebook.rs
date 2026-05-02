//! Static codebook + VLC table placeholders for the SVQ1 hierarchical
//! multistage VQ.
//!
//! ## Status — VQ tables not in scope of the round-1 decoder
//!
//! `docs/video/svq1/svq1-trace-reverse-engineering.md` is explicit
//! that the per-level multistage VLC tables (six intra + six inter),
//! the intra/inter mean VLC tables, the H.263-style motion-component
//! VLC, and the **6 levels × 6 stages × 16 entries** of fixed signed-
//! 8-bit codebook bytes "are not reproduced here … must be reverse-
//! engineered from a reference decoder". Workspace policy further
//! prohibits copying any external library source verbatim.
//!
//! Until the tables are independently reverse-engineered into a
//! workspace-local `docs/video/svq1/svq1-tables.md`, the VQ walker
//! ([`crate::vq`]) operates in **mean-only fallback mode**: it parses
//! every bit of the body using the syntactic structure described in
//! the trace doc, but synthesises each leaf as a flat fill at the
//! per-frame midpoint (`128` for INTRA, `0` for INTER added on top of
//! a zeroed reference). This is enough to:
//!
//! * exhaust the body bits — i.e. the bit position at end-of-frame
//!   matches what the encoder wrote;
//! * produce a recognisable low-pass blurred preview of the source;
//! * exercise the decoder API end-to-end against ffmpeg-encoded
//!   fixtures and yield a measurable PSNR for the test harness.
//!
//! See the crate-level `Gaps` section (lib.rs) for the path to a
//! full-codebook decode.

/// Number of VQ levels (5 down to 0).
pub const NUM_LEVELS: usize = 6;

/// Number of multistage codebook stages per level. The trace reports
/// `count` ranging from `0..=6` (and `−1` only on INTER skips), so the
/// codebook is sized for six stages.
pub const NUM_STAGES: usize = 6;

/// Number of entries per stage — `count × u(4)` indices imply 16.
pub const NUM_ENTRIES_PER_STAGE: usize = 16;

/// Pixel count per leaf at level `L`. Width × height per the trace's
/// §7 size table:
///
/// | Level | width | height | pixels |
/// |-------|-------|--------|--------|
/// | 0     | 4     | 2      |    8   |
/// | 1     | 4     | 4      |   16   |
/// | 2     | 8     | 4      |   32   |
/// | 3     | 8     | 8      |   64   |
/// | 4     | 16    | 8      |  128   |
/// | 5     | 16    | 16     |  256   |
pub const fn pixels_at_level(level: u8) -> usize {
    LEAF_DIMS[level as usize].0 * LEAF_DIMS[level as usize].1
}

/// `(width, height)` of a sub-block at each level. See the §7 table
/// in the trace doc.
pub const LEAF_DIMS: [(usize, usize); NUM_LEVELS] = [
    (4, 2),   // L=0
    (4, 4),   // L=1
    (8, 4),   // L=2
    (8, 8),   // L=3
    (16, 8),  // L=4
    (16, 16), // L=5
];

/// Maximum hierarchical level — a top-of-MB sub-block.
pub const MAX_LEVEL: u8 = 5;

/// Per-trace §7: only levels 0..=3 may carry stage counts > 0. At
/// levels 4 and 5 the bitstream MUST encode a flat-mean fill
/// (`stages == 0`) or an INTER skip (`stages == -1`); higher counts
/// are an invalid-data error in a conformant decoder.
pub const fn max_stages_at_level(level: u8) -> u8 {
    match level {
        0..=3 => NUM_STAGES as u8,
        _ => 0,
    }
}
