//! SVQ3 macroblock-level intra predictor-selection + reconstruction
//! loop.
//!
//! This module assembles the leaf primitives the earlier rounds landed
//! — the 4×4 intra predictors + mode dispatcher
//! ([`crate::svq3_pred::predict_intra_4x4`]), the predicted+residual
//! writeback ([`crate::svq3_pred::reconstruct_4x4`]), and the
//! per-sub-block scan order ([`crate::svq3_mb::INTRA_4X4_SCAN_ORDER`])
//! — into the **macroblock-level predictor-selection loop** that the
//! README named as the open lacks-tail
//! (`docs/video/svq3/spec/01-reconstruction-composition.md` Gaps 3/4/5,
//! plus the wiki §"Intra macroblock information decoding" scan order).
//!
//! ## Wall / provenance
//!
//! Everything here is composition of already-pinned primitives plus the
//! wiki's explicit 4×4 sub-block processing order. The genuinely-pinned
//! facts consumed:
//!
//! * The sub-block processing order is the wiki §"Intra macroblock
//!   information decoding" picture (mirrored as
//!   [`crate::svq3_mb::INTRA_4X4_SCAN_ORDER`]); its spatial layout
//!   (which 4×4 cell each block index occupies inside the 16×16
//!   macroblock) is the [`LUMA_BLOCK_GRID_POS`] map derived directly
//!   from that picture.
//! * Per-sub-block neighbour assembly (top row + left column + corner)
//!   is the standard H.264 reconstructed-neighbour read that the 4×4
//!   predictors (`docs/video/svq3/spec/01` Gap 3/4) consume; the
//!   availability flags follow the macroblock-edge rule (a sub-block on
//!   the macroblock's top/left edge with no out-of-MB neighbour
//!   supplied is treated as having that neighbour unavailable).
//! * The predicted+residual writeback is Gap 5's saturating
//!   `Clip1(pred + residual)`.
//!
//! ## What this loop does NOT do (genuine docs gaps)
//!
//! ## Two reconstruction entry points
//!
//! [`reconstruct_intra_luma_macroblock`] is **residual-provider-driven**:
//! the caller supplies each sub-block's already-dequantised +
//! inverse-transformed residual `[i32; 16]` block. As of spec/01 Gap 2
//! the residual pipeline (place → dequant · `svq3_dequant_coeff[Q]` →
//! two-sided `M·X·Mᵀ` transform → fused `+0x80000 >> 20`) is now pinned
//! as an unambiguous per-element formula
//! ([`crate::svq3_dequant::dequantize_transform_luma_block`]), so
//! [`reconstruct_intra_luma_macroblock_from_coeffs`] now owns the
//! **full** per-block composition: it takes the placed coefficient grids
//! plus the slice quantiser, runs the residual interleave internally,
//! then drives the same predictor-selection / neighbour-sequencing /
//! writeback loop.
//!
//! Inter (motion-compensated) macroblocks, CBP-driven residual
//! presence, and the intra-mode VLC wire decode are still gated on their
//! own docs gaps (the wiki states only "CBP is coded the same way as in
//! H.264" / "motion vector differences are coded as signed
//! variable-length codes" without enumerating the H.264 CBP code-number
//! mapping or the MV-VLC bit layout) and are not driven here.

use crate::svq3_dequant::dequantize_transform_luma_block;
use crate::svq3_pred::{
    predict_intra_4x4, reconstruct_4x4, Intra4x4Neighbours, Svq3IntraMode, PRED_4X4_DIM,
    PRED_4X4_SAMPLES,
};

/// Side length of a luma macroblock in pixels.
pub const MB_LUMA_DIM: usize = 16;

/// Side length of the macroblock measured in 4×4 sub-blocks
/// (`MB_LUMA_DIM / 4 = 4`).
pub const MB_GRID_DIM: usize = MB_LUMA_DIM / PRED_4X4_DIM;

/// Number of 4×4 luma sub-blocks in one macroblock (`4 × 4 = 16`).
pub const MB_LUMA_BLOCKS: usize = MB_GRID_DIM * MB_GRID_DIM;

/// Spatial grid position `(grid_row, grid_col)` (each in `0..=3`,
/// in units of 4×4 cells) of each luma 4×4 sub-block **index** inside
/// the 16×16 macroblock.
///
/// Derived directly from the wiki §"Intra macroblock information
/// decoding" picture that [`crate::svq3_mb::INTRA_4X4_SCAN_ORDER`]
/// mirrors:
///
/// ```text
///   ( 0,  1)  ( 4,  5)
///   ( 2,  3)  ( 6,  7)
///   ( 8,  9)  (12, 13)
///   (10, 11)  (14, 15)
/// ```
///
/// The picture lays the block **indices** out at their spatial grid
/// cells: grid cell `(0,0)` holds block `0`, `(0,1)` holds block `1`,
/// `(0,2)` holds block `4`, `(0,3)` holds block `5`, and so on. This
/// table is the index→position inverse: `LUMA_BLOCK_GRID_POS[index]`
/// gives the `(grid_row, grid_col)` cell that block `index` occupies.
/// The pixel origin of block `index` is therefore
/// `(grid_row * 4, grid_col * 4)`.
pub const LUMA_BLOCK_GRID_POS: [(usize, usize); MB_LUMA_BLOCKS] = {
    // Transcribe the picture cell-by-cell: picture[grid_row][grid_col]
    // = block index at that spatial cell.
    let picture: [[usize; MB_GRID_DIM]; MB_GRID_DIM] =
        [[0, 1, 4, 5], [2, 3, 6, 7], [8, 9, 12, 13], [10, 11, 14, 15]];
    // Invert: pos[index] = (grid_row, grid_col).
    let mut pos = [(0usize, 0usize); MB_LUMA_BLOCKS];
    let mut gr = 0;
    while gr < MB_GRID_DIM {
        let mut gc = 0;
        while gc < MB_GRID_DIM {
            pos[picture[gr][gc]] = (gr, gc);
            gc += 1;
        }
        gr += 1;
    }
    pos
};

/// A 16×16 luma macroblock plane being reconstructed, plus the
/// out-of-macroblock neighbour samples (the reconstructed pixels from
/// the macroblocks above and to the left) the edge sub-blocks read.
///
/// The plane is stored row-major (`samples[y * 16 + x]`). The
/// out-of-MB neighbour rows/columns let the first sub-block row read
/// the macroblock above and the first sub-block column read the
/// macroblock to the left; their availability mirrors whether such a
/// neighbour macroblock exists in the slice.
#[derive(Debug, Clone)]
pub struct LumaMacroblock {
    /// Reconstructed luma samples, row-major, `16 × 16`.
    pub samples: [u8; MB_LUMA_DIM * MB_LUMA_DIM],
    /// The 16 reconstructed samples of the row directly above the
    /// macroblock (`above[x]` = pixel at `(x, -1)`), left-to-right.
    /// Only read when [`Self::above_available`] is set.
    pub above: [u8; MB_LUMA_DIM],
    /// The 16 reconstructed samples of the column directly to the left
    /// of the macroblock (`leftcol[y]` = pixel at `(-1, y)`),
    /// top-to-bottom. Only read when [`Self::left_available`] is set.
    pub leftcol: [u8; MB_LUMA_DIM],
    /// The reconstructed corner sample at `(-1, -1)` (above-left of the
    /// macroblock). Read by a top-left sub-block's diagonal predictor
    /// when both edges are available.
    pub corner: u8,
    /// Whether a macroblock exists above this one (so [`Self::above`]
    /// is meaningful).
    pub above_available: bool,
    /// Whether a macroblock exists to the left of this one (so
    /// [`Self::leftcol`] is meaningful).
    pub left_available: bool,
}

impl LumaMacroblock {
    /// A macroblock with all samples / neighbours zeroed and both
    /// out-of-MB neighbours marked unavailable (a top-left macroblock).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            samples: [0u8; MB_LUMA_DIM * MB_LUMA_DIM],
            above: [0u8; MB_LUMA_DIM],
            leftcol: [0u8; MB_LUMA_DIM],
            corner: 0,
            above_available: false,
            left_available: false,
        }
    }

    /// Read the reconstructed sample at macroblock-relative pixel
    /// `(x, y)` (both `0..=15`).
    #[inline]
    #[must_use]
    pub const fn sample(&self, x: usize, y: usize) -> u8 {
        self.samples[y * MB_LUMA_DIM + x]
    }

    /// Assemble the [`Intra4x4Neighbours`] for the 4×4 sub-block whose
    /// pixel origin is `(bx, by)` (each a multiple of 4 in `0..=12`).
    ///
    /// The top row reads pixels `(bx + i, by - 1)` and the left column
    /// reads `(bx - 1, by + i)` for `i ∈ 0..=3`; the corner reads
    /// `(bx - 1, by - 1)`. When `by == 0` the top row comes from the
    /// macroblock above ([`Self::above`]); when `bx == 0` the left
    /// column comes from the macroblock to the left ([`Self::leftcol`]).
    /// Availability propagates from the out-of-MB neighbour flags for
    /// those edge sub-blocks and is always `true` for interior
    /// sub-blocks (whose neighbours are previously-reconstructed
    /// in-MB pixels).
    #[must_use]
    pub fn neighbours_at(&self, bx: usize, by: usize) -> Intra4x4Neighbours {
        let mut top = [0u8; PRED_4X4_DIM];
        let mut left = [0u8; PRED_4X4_DIM];

        let top_available = by > 0 || self.above_available;
        let left_available = bx > 0 || self.left_available;

        // Top row: (bx + i, by - 1).
        for (i, t) in top.iter_mut().enumerate() {
            *t = if by > 0 {
                self.sample(bx + i, by - 1)
            } else {
                self.above[bx + i]
            };
        }
        // Left column: (bx - 1, by + i).
        for (i, l) in left.iter_mut().enumerate() {
            *l = if bx > 0 {
                self.sample(bx - 1, by + i)
            } else {
                self.leftcol[by + i]
            };
        }
        // Corner: (bx - 1, by - 1).
        let corner = match (bx > 0, by > 0) {
            (true, true) => self.sample(bx - 1, by - 1),
            (false, true) => {
                // left edge of MB, interior row: corner is the leftcol
                // sample one row up.
                if self.left_available {
                    self.leftcol[by - 1]
                } else {
                    0
                }
            }
            (true, false) => {
                // top edge of MB, interior col: corner is the above-row
                // sample one col left.
                if self.above_available {
                    self.above[bx - 1]
                } else {
                    0
                }
            }
            (false, false) => self.corner,
        };

        Intra4x4Neighbours {
            top,
            left,
            corner,
            top_available,
            left_available,
        }
    }

    /// Write a reconstructed 4×4 block (`block[r * 4 + c]`) into the
    /// plane at pixel origin `(bx, by)`.
    pub fn write_block(&mut self, bx: usize, by: usize, block: [u8; PRED_4X4_SAMPLES]) {
        for r in 0..PRED_4X4_DIM {
            for c in 0..PRED_4X4_DIM {
                self.samples[(by + r) * MB_LUMA_DIM + (bx + c)] = block[r * PRED_4X4_DIM + c];
            }
        }
    }
}

impl Default for LumaMacroblock {
    fn default() -> Self {
        Self::new()
    }
}

/// Reconstruct one 16×16 luma macroblock's 4×4-intra sub-blocks,
/// driving predictor selection + residual writeback in the wiki's
/// documented sub-block processing order.
///
/// This is the **macroblock-level predictor-selection loop**. For each
/// of the 16 luma 4×4 sub-blocks, walked in the wiki order
/// ([`crate::svq3_mb::INTRA_4X4_SCAN_ORDER`]):
///
/// 1. The sub-block's spatial cell is looked up in
///    [`LUMA_BLOCK_GRID_POS`] to get its pixel origin `(bx, by)`.
/// 2. Its neighbour samples are assembled from the *already
///    reconstructed* pixels of earlier sub-blocks (and the out-of-MB
///    neighbour rows for edge sub-blocks) via
///    [`LumaMacroblock::neighbours_at`].
/// 3. The resolved per-sub-block intra mode (`modes[index]`) selects a
///    predictor via [`predict_intra_4x4`].
/// 4. The predicted block is combined with the caller-supplied residual
///    block (`residuals[index]`) via [`reconstruct_4x4`] (Gap 5's
///    `Clip1(pred + residual)`), and the result is written back into
///    the plane so later sub-blocks in the scan can read it as a
///    neighbour.
///
/// `modes[index]` / `residuals[index]` are indexed by the **block
/// index** (raster `0..=15`, the same index space the wiki picture and
/// [`LUMA_BLOCK_GRID_POS`] use), not by scan position — the loop maps
/// scan position → block index internally.
///
/// The residual blocks are supplied by the caller because the residual
/// pipeline (dequant · transform · shift) is a deferred docs-gap (see
/// the module docs); this loop owns predictor selection, neighbour
/// sequencing, and writeback only. On return `mb.samples` holds the
/// fully reconstructed 16×16 luma plane.
pub fn reconstruct_intra_luma_macroblock(
    mb: &mut LumaMacroblock,
    modes: &[Svq3IntraMode; MB_LUMA_BLOCKS],
    residuals: &[[i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS],
) {
    for &scan_index in crate::svq3_mb::INTRA_4X4_SCAN_ORDER.iter() {
        let index = scan_index as usize;
        let (gr, gc) = LUMA_BLOCK_GRID_POS[index];
        let by = gr * PRED_4X4_DIM;
        let bx = gc * PRED_4X4_DIM;

        let nb = mb.neighbours_at(bx, by);
        let predicted = predict_intra_4x4(modes[index], nb);
        let recon = reconstruct_4x4(predicted, residuals[index]);
        mb.write_block(bx, by, recon);
    }
}

/// Reconstruct one 16×16 luma macroblock's 4×4-intra sub-blocks
/// **end-to-end from placed coefficient grids** — the full per-block
/// reconstruction composition.
///
/// This is the residual-owning counterpart to
/// [`reconstruct_intra_luma_macroblock`]: rather than the caller
/// supplying pre-computed `[i32; 16]` residuals, each sub-block's
/// **placed coefficient grid** (`coeff_blocks[index]`, the row-major
/// dezigzagged output of [`crate::svq3_scan::place_4x4`]) is run through
/// the spec/01 Gap 2 residual interleave
/// [`crate::svq3_dequant::dequantize_transform_luma_block`] (per-element
/// `out = (coeff·DEQUANT_COEFF_TABLE[Q] + 0x80000) >> 20` with the
/// two-sided `M·X·Mᵀ` transform folded in) at the slice quantiser `q`,
/// then the same predictor-selection / neighbour-sequencing / writeback
/// loop runs.
///
/// `modes[index]` / `coeff_blocks[index]` are indexed by the **block
/// index** (raster `0..=15`, the index space the wiki picture and
/// [`LUMA_BLOCK_GRID_POS`] use). The `q` argument is the slice
/// quantiser; it must satisfy `q < DEQUANT_COEFF_TABLE_LEN`. On return
/// `mb.samples` holds the fully reconstructed 16×16 luma plane.
///
/// This entry point covers the no-separate-DC luma case (`dc = 0`); the
/// separate-DC-block branch (where `dc = INTRA_LUMA_DC_SCALE · dc_block[i]`
/// is folded in per sub-block) layers on top of
/// [`crate::svq3_dequant::dequantize_transform_luma_block_with_dc`] once
/// the separate-DC presence is decoded from the (still-deferred) CBP /
/// MB-type wire format.
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
pub fn reconstruct_intra_luma_macroblock_from_coeffs(
    mb: &mut LumaMacroblock,
    modes: &[Svq3IntraMode; MB_LUMA_BLOCKS],
    coeff_blocks: &[[i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS],
    q: u32,
) {
    for &scan_index in crate::svq3_mb::INTRA_4X4_SCAN_ORDER.iter() {
        let index = scan_index as usize;
        let (gr, gc) = LUMA_BLOCK_GRID_POS[index];
        let by = gr * PRED_4X4_DIM;
        let bx = gc * PRED_4X4_DIM;

        // Spec/01 Gap 2 residual interleave: place → dequant·scale →
        // two-sided transform → fused +0x80000 >>20.
        let residual = dequantize_transform_luma_block(q, coeff_blocks[index]);

        let nb = mb.neighbours_at(bx, by);
        let predicted = predict_intra_4x4(modes[index], nb);
        let recon = reconstruct_4x4(predicted, residual);
        mb.write_block(bx, by, recon);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_constants() {
        assert_eq!(MB_LUMA_DIM, 16);
        assert_eq!(MB_GRID_DIM, 4);
        assert_eq!(MB_LUMA_BLOCKS, 16);
    }

    #[test]
    fn grid_pos_is_inverse_of_wiki_picture() {
        // The wiki picture, block index per spatial cell.
        let picture: [[usize; 4]; 4] =
            [[0, 1, 4, 5], [2, 3, 6, 7], [8, 9, 12, 13], [10, 11, 14, 15]];
        for (gr, row) in picture.iter().enumerate() {
            for (gc, &index) in row.iter().enumerate() {
                assert_eq!(LUMA_BLOCK_GRID_POS[index], (gr, gc), "block {index}");
            }
        }
    }

    #[test]
    fn grid_pos_is_a_permutation_of_all_cells() {
        let mut seen = [false; MB_LUMA_BLOCKS];
        for &(gr, gc) in LUMA_BLOCK_GRID_POS.iter() {
            assert!(gr < MB_GRID_DIM && gc < MB_GRID_DIM);
            let flat = gr * MB_GRID_DIM + gc;
            assert!(!seen[flat], "cell ({gr},{gc}) used twice");
            seen[flat] = true;
        }
        assert!(seen.iter().all(|&s| s), "not all grid cells covered");
    }

    #[test]
    fn write_then_sample_round_trips() {
        let mut mb = LumaMacroblock::new();
        let mut block = [0u8; 16];
        for (i, b) in block.iter_mut().enumerate() {
            *b = (i * 3) as u8;
        }
        // Write at pixel origin (8, 4).
        mb.write_block(8, 4, block);
        for r in 0..4 {
            for c in 0..4 {
                assert_eq!(mb.sample(8 + c, 4 + r), block[r * 4 + c]);
            }
        }
    }

    #[test]
    fn interior_subblock_neighbours_are_in_mb_and_available() {
        let mut mb = LumaMacroblock::new();
        // Fill the whole plane with a known ramp so we can verify reads.
        for y in 0..16 {
            for x in 0..16 {
                mb.samples[y * 16 + x] = (x + y) as u8;
            }
        }
        // Interior sub-block at (bx=4, by=4).
        let nb = mb.neighbours_at(4, 4);
        assert!(nb.top_available);
        assert!(nb.left_available);
        // Top row reads (4+i, 3): values 7,8,9,10.
        assert_eq!(nb.top, [7, 8, 9, 10]);
        // Left column reads (3, 4+i): values 7,8,9,10.
        assert_eq!(nb.left, [7, 8, 9, 10]);
        // Corner reads (3, 3) = 6.
        assert_eq!(nb.corner, 6);
    }

    #[test]
    fn top_left_subblock_uses_out_of_mb_neighbours() {
        let mut mb = LumaMacroblock::new();
        mb.above = [100; 16];
        mb.leftcol = [50; 16];
        mb.corner = 200;
        mb.above_available = true;
        mb.left_available = true;
        let nb = mb.neighbours_at(0, 0);
        assert!(nb.top_available);
        assert!(nb.left_available);
        assert_eq!(nb.top, [100; 4]);
        assert_eq!(nb.left, [50; 4]);
        assert_eq!(nb.corner, 200);
    }

    #[test]
    fn edge_subblock_availability_follows_out_of_mb_flags() {
        let mb = LumaMacroblock::new(); // both neighbours unavailable
                                        // Top-left sub-block: neither neighbour available.
        let nb = mb.neighbours_at(0, 0);
        assert!(!nb.top_available);
        assert!(!nb.left_available);
        // A sub-block on the top edge but interior column (bx=4, by=0):
        // top from above (unavailable), left from in-MB (available).
        let nb2 = mb.neighbours_at(4, 0);
        assert!(!nb2.top_available);
        assert!(nb2.left_available);
        // Left edge, interior row (bx=0, by=4): top in-MB, left from
        // leftcol (unavailable).
        let nb3 = mb.neighbours_at(0, 4);
        assert!(nb3.top_available);
        assert!(!nb3.left_available);
    }

    #[test]
    fn dc_only_macroblock_with_zero_residual_is_flat_128() {
        // A top-left macroblock with no neighbours: every DC sub-block
        // predicts 128, and with zero residual the whole plane is 128.
        let mut mb = LumaMacroblock::new();
        let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];
        let residuals = [[0i32; 16]; MB_LUMA_BLOCKS];
        reconstruct_intra_luma_macroblock(&mut mb, &modes, &residuals);
        assert!(mb.samples.iter().all(|&s| s == 128), "expected flat 128");
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn vertical_macroblock_propagates_above_row_down_each_column() {
        // A macroblock with an available `above` row, all sub-blocks in
        // vertical mode, zero residual: each column should carry the
        // matching `above` sample all the way down (the first sub-block
        // row copies `above`, and later rows copy the reconstructed row
        // directly above — which equals `above` — so the whole column
        // is constant).
        let mut mb = LumaMacroblock::new();
        let mut above = [0u8; 16];
        for (x, a) in above.iter_mut().enumerate() {
            *a = (x * 8) as u8; // 0,8,16,...,120
        }
        mb.above = above;
        mb.above_available = true;
        // left must be available too for vertical to not matter; but
        // vertical only needs top. Keep left unavailable.
        let modes = [Svq3IntraMode::Vertical; MB_LUMA_BLOCKS];
        let residuals = [[0i32; 16]; MB_LUMA_BLOCKS];
        reconstruct_intra_luma_macroblock(&mut mb, &modes, &residuals);
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(mb.sample(x, y), above[x], "({x},{y})");
            }
        }
    }

    #[test]
    fn residual_is_added_after_prediction() {
        // DC macroblock (flat 128) plus a per-block residual that bumps
        // block 0's first sample by 10.
        let mut mb = LumaMacroblock::new();
        let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];
        let mut residuals = [[0i32; 16]; MB_LUMA_BLOCKS];
        residuals[0][0] = 10; // block 0 is at grid (0,0) → pixel (0,0)
        reconstruct_intra_luma_macroblock(&mut mb, &modes, &residuals);
        assert_eq!(mb.sample(0, 0), 138);
        // Neighbouring sample untouched.
        assert_eq!(mb.sample(1, 0), 128);
    }

    // ---- End-to-end from coefficient grids (spec/01 Gap 2 + loop) ------

    #[test]
    fn from_coeffs_zero_coefficients_matches_zero_residual_path() {
        // All-zero coefficient grids ⇒ all-zero residuals ⇒ identical to
        // the residual-provider path with zero residuals: flat-128 DC MB.
        let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];
        let coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];

        let mut mb_a = LumaMacroblock::new();
        reconstruct_intra_luma_macroblock_from_coeffs(&mut mb_a, &modes, &coeffs, 12);

        let mut mb_b = LumaMacroblock::new();
        let residuals = [[0i32; 16]; MB_LUMA_BLOCKS];
        reconstruct_intra_luma_macroblock(&mut mb_b, &modes, &residuals);

        assert_eq!(mb_a.samples, mb_b.samples);
        assert!(mb_a.samples.iter().all(|&s| s == 128));
    }

    #[test]
    fn from_coeffs_equals_manual_residual_interleave_then_loop() {
        // Drive the end-to-end path and the explicit
        // (per-block residual interleave → residual-provider loop) path
        // on the same coefficient grids; they must agree exactly.
        use crate::svq3_dequant::dequantize_transform_luma_block;

        let q = 9;
        let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];
        let mut coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];
        // Seed a few blocks with structured coefficients.
        for (bi, blk) in coeffs.iter_mut().enumerate() {
            for (ci, c) in blk.iter_mut().enumerate() {
                *c = ((bi as i32) * 2 + (ci as i32) - 8) % 5;
            }
        }

        let mut mb_e2e = LumaMacroblock::new();
        reconstruct_intra_luma_macroblock_from_coeffs(&mut mb_e2e, &modes, &coeffs, q);

        let mut residuals = [[0i32; 16]; MB_LUMA_BLOCKS];
        for (i, r) in residuals.iter_mut().enumerate() {
            *r = dequantize_transform_luma_block(q, coeffs[i]);
        }
        let mut mb_manual = LumaMacroblock::new();
        reconstruct_intra_luma_macroblock(&mut mb_manual, &modes, &residuals);

        assert_eq!(mb_e2e.samples, mb_manual.samples);
    }

    #[test]
    fn from_coeffs_pure_dc_block_shifts_flat_prediction() {
        // A single pure-DC coefficient in block 0 produces a flat
        // residual across that block, lifting the DC-predicted 128 plane
        // uniformly over block 0's 4×4 footprint.
        use crate::svq3_dequant::dequantize_transform_luma_block;
        let q = 14;
        let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];
        let mut coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];
        coeffs[0][0] = 1; // pure DC in block 0 (grid (0,0) → pixel (0,0))

        let mut mb = LumaMacroblock::new();
        reconstruct_intra_luma_macroblock_from_coeffs(&mut mb, &modes, &coeffs, q);

        let residual = dequantize_transform_luma_block(q, coeffs[0]);
        let expected = (128 + residual[0]).clamp(0, 255) as u8;
        // Block 0 footprint is pixels (0..4, 0..4); all flat == expected.
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(mb.sample(x, y), expected, "({x},{y})");
            }
        }
        // The single non-zero coefficient was in block 0, so block 0's
        // reconstruction differs from the bare flat-128 DC prediction.
        assert_ne!(expected, 128, "pure-DC residual must move the plane");
    }

    #[test]
    #[should_panic]
    fn from_coeffs_panics_on_out_of_range_quantiser() {
        use crate::svq3_dequant::DEQUANT_COEFF_TABLE_LEN;
        let mut mb = LumaMacroblock::new();
        let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];
        let coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];
        reconstruct_intra_luma_macroblock_from_coeffs(
            &mut mb,
            &modes,
            &coeffs,
            DEQUANT_COEFF_TABLE_LEN as u32,
        );
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn scan_order_dependency_block5_reads_block0_through_block1() {
        // Sanity: the loop must reconstruct earlier-scan sub-blocks
        // before a later one reads them. Use horizontal mode so each
        // sub-block copies its left column. Provide an available left
        // macroblock with a distinctive ramp; with zero residual every
        // row should equal its leftcol seed, propagated rightward.
        let mut mb = LumaMacroblock::new();
        let mut leftcol = [0u8; 16];
        for (y, l) in leftcol.iter_mut().enumerate() {
            *l = (y * 4) as u8; // 0,4,8,...,60
        }
        mb.leftcol = leftcol;
        mb.left_available = true;
        let modes = [Svq3IntraMode::Horizontal; MB_LUMA_BLOCKS];
        let residuals = [[0i32; 16]; MB_LUMA_BLOCKS];
        reconstruct_intra_luma_macroblock(&mut mb, &modes, &residuals);
        // Horizontal copies the left column across each row; the left
        // column of the first sub-block column is the leftcol seed, and
        // it propagates rightward, so every pixel in row y equals
        // leftcol[y].
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(mb.sample(x, y), leftcol[y], "({x},{y})");
            }
        }
    }
}
