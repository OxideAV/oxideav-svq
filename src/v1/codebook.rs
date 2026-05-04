//! Static codebook geometry + level-stage caps for SVQ1's hierarchical
//! multistage VQ. The actual byte tables live in [`crate::v1::tables`].
//!
//! Per §7 of `docs/video/svq1/svq1-trace-reverse-engineering.md` the
//! tree has 6 levels (5 down to 0). Each level has a fixed sub-block
//! size. The trace doc claims (in §7 lines 416–431, and again in
//! §14.7) that only levels 0..3 may carry stage counts > 0; round 3
//! disproved this empirically — see the round-3 CHANGELOG entry and
//! `tests/trace_first_mbs.rs`. The L=4 (16×8) and L=5 (16×16)
//! codebooks really do exist in the FFmpeg encoder/decoder, and their
//! bytes are an outstanding docs-collaborator deliverable.
//!
//! ## Codebook lookup
//!
//! Use [`intra_codebook_for_level`] / [`inter_codebook_for_level`] to
//! get a slice-of-stages-of-entries view for a given leaf level.
//! Level 0 = 4×2 (8 bytes/entry), level 1 = 4×4 (16), level 2 = 8×4
//! (32), level 3 = 8×8 (64). Levels 4 (16×8) and 5 (16×16) return
//! `None` until the trace doc supplies their bytes — until then the
//! walker keeps the bit-reader aligned at L=4/5 leaves with
//! `count > 0` by consuming the codebook-index bits (4 per stage)
//! but skipping the additive contribution. This costs ~10 dB Y PSNR
//! on the testsrc roundtrip; see [`crate::v1::vq::decode_intra_leaf`]
//! for the in-tree call site.

use super::tables::{
    INTER_CB_4X2, INTER_CB_4X4, INTER_CB_8X4, INTER_CB_8X8, INTRA_CB_4X2, INTRA_CB_4X4,
    INTRA_CB_8X4, INTRA_CB_8X8,
};

/// Number of VQ levels (5 down to 0).
pub const NUM_LEVELS: usize = 6;

/// Number of multistage codebook stages per level. The trace reports
/// `count` ranging from `0..=6` (and `−1` only on INTER skips), so the
/// codebook is sized for six stages.
pub const NUM_STAGES: usize = 6;

/// Number of entries per stage — `count × u(4)` indices imply 16.
pub const NUM_ENTRIES_PER_STAGE: usize = 16;

/// Pixel count per leaf at level `L`. Width × height per the trace's
/// §7 size table.
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

/// At each non-zero level, the parent splits along an alternating
/// axis. Returns `true` if the split at level `L` halves the **height**
/// (i.e. produces a top + bottom child pair); `false` if it halves the
/// **width** (left + right child pair). Per §7 of the trace doc this
/// is "horizontal at odd levels, vertical at even levels" — meaning
/// the split *line* is horizontal at odd L (so height is halved) and
/// vertical at even L (so width is halved). Verified by inspecting
/// the level → size table:
///
/// * L=5 (odd) split: 16×16 → 16×8 + 16×8 (height halved → horizontal line).
/// * L=4 (even) split: 16×8 → 8×8 + 8×8 (width halved → vertical line).
/// * L=3 (odd) split: 8×8 → 8×4 + 8×4 (height halved).
/// * L=2 (even) split: 8×4 → 4×4 + 4×4 (width halved).
/// * L=1 (odd) split: 4×4 → 4×2 + 4×2 (height halved).
pub const fn split_halves_height(level: u8) -> bool {
    level % 2 == 1
}

/// Variants of the codebook by leaf size. Used internally to abstract
/// over the differently-sized 2-D arrays.
pub enum LeafCodebook {
    /// 4×2: 8 bytes/entry.
    L4x2(&'static [[[i8; 8]; 16]; 6]),
    /// 4×4: 16 bytes/entry.
    L4x4(&'static [[[i8; 16]; 16]; 6]),
    /// 8×4: 32 bytes/entry.
    L8x4(&'static [[[i8; 32]; 16]; 6]),
    /// 8×8: 64 bytes/entry.
    L8x8(&'static [[[i8; 64]; 16]; 6]),
}

impl LeafCodebook {
    /// Number of pixels in this codebook's leaf.
    pub fn pixels(&self) -> usize {
        match self {
            LeafCodebook::L4x2(_) => 8,
            LeafCodebook::L4x4(_) => 16,
            LeafCodebook::L8x4(_) => 32,
            LeafCodebook::L8x8(_) => 64,
        }
    }

    /// Read entry `idx` (0..16) at stage `stage` (0..6) into `out`.
    /// `out.len()` must equal `self.pixels()`.
    pub fn copy_into(&self, stage: usize, idx: usize, out: &mut [i32]) {
        debug_assert_eq!(out.len(), self.pixels());
        match self {
            LeafCodebook::L4x2(cb) => {
                for (o, b) in out.iter_mut().zip(cb[stage][idx].iter()) {
                    *o = *b as i32;
                }
            }
            LeafCodebook::L4x4(cb) => {
                for (o, b) in out.iter_mut().zip(cb[stage][idx].iter()) {
                    *o = *b as i32;
                }
            }
            LeafCodebook::L8x4(cb) => {
                for (o, b) in out.iter_mut().zip(cb[stage][idx].iter()) {
                    *o = *b as i32;
                }
            }
            LeafCodebook::L8x8(cb) => {
                for (o, b) in out.iter_mut().zip(cb[stage][idx].iter()) {
                    *o = *b as i32;
                }
            }
        }
    }

    /// Add entry `idx` at stage `stage` to `out` (in-place accumulate).
    pub fn add_into(&self, stage: usize, idx: usize, out: &mut [i32]) {
        debug_assert_eq!(out.len(), self.pixels());
        match self {
            LeafCodebook::L4x2(cb) => {
                for (o, b) in out.iter_mut().zip(cb[stage][idx].iter()) {
                    *o += *b as i32;
                }
            }
            LeafCodebook::L4x4(cb) => {
                for (o, b) in out.iter_mut().zip(cb[stage][idx].iter()) {
                    *o += *b as i32;
                }
            }
            LeafCodebook::L8x4(cb) => {
                for (o, b) in out.iter_mut().zip(cb[stage][idx].iter()) {
                    *o += *b as i32;
                }
            }
            LeafCodebook::L8x8(cb) => {
                for (o, b) in out.iter_mut().zip(cb[stage][idx].iter()) {
                    *o += *b as i32;
                }
            }
        }
    }
}

/// Look up the **intra** codebook for a given leaf level (0..=3).
/// Returns `None` for levels 4 and 5 (which must be mean-only).
pub fn intra_codebook_for_level(level: u8) -> Option<LeafCodebook> {
    match level {
        0 => Some(LeafCodebook::L4x2(INTRA_CB_4X2)),
        1 => Some(LeafCodebook::L4x4(INTRA_CB_4X4)),
        2 => Some(LeafCodebook::L8x4(INTRA_CB_8X4)),
        3 => Some(LeafCodebook::L8x8(INTRA_CB_8X8)),
        _ => None,
    }
}

/// Look up the **inter** codebook for a given leaf level (0..=3).
/// Returns `None` for levels 4 and 5.
pub fn inter_codebook_for_level(level: u8) -> Option<LeafCodebook> {
    match level {
        0 => Some(LeafCodebook::L4x2(INTER_CB_4X2)),
        1 => Some(LeafCodebook::L4x4(INTER_CB_4X4)),
        2 => Some(LeafCodebook::L8x4(INTER_CB_8X4)),
        3 => Some(LeafCodebook::L8x8(INTER_CB_8X8)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_at_level_matches_dims() {
        for l in 0..NUM_LEVELS as u8 {
            let (w, h) = LEAF_DIMS[l as usize];
            assert_eq!(pixels_at_level(l), w * h);
        }
    }

    #[test]
    fn split_axis_alternates() {
        // Odd levels halve height, even levels halve width.
        assert!(split_halves_height(5));
        assert!(!split_halves_height(4));
        assert!(split_halves_height(3));
        assert!(!split_halves_height(2));
        assert!(split_halves_height(1));
    }

    #[test]
    fn codebook_lookup_returns_correct_pixel_count() {
        for &(level, expected_px) in &[(0, 8), (1, 16), (2, 32), (3, 64)] {
            let cb = intra_codebook_for_level(level).unwrap();
            assert_eq!(cb.pixels(), expected_px);
            let cb = inter_codebook_for_level(level).unwrap();
            assert_eq!(cb.pixels(), expected_px);
        }
        assert!(intra_codebook_for_level(4).is_none());
        assert!(intra_codebook_for_level(5).is_none());
    }
}
