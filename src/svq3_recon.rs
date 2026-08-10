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

use crate::svq3_dequant::{
    chroma_quantiser_index, dequantize_chroma_dc_levels, dequantize_transform_intra_luma_block,
    dequantize_transform_luma_block, dequantize_transform_luma_block_with_dc,
};
use crate::svq3_pred::{
    predict_chroma_dc_8x8, predict_dc_16x16, predict_horizontal_16x16, predict_intra_4x4,
    predict_plane_16x16, predict_vertical_16x16, reconstruct_4x4, reconstruct_sample,
    Intra4x4Neighbours, Svq3IntraMode, PRED_16X16_DIM, PRED_4X4_DIM, PRED_4X4_SAMPLES,
    PRED_CHROMA_DIM, PRED_CHROMA_SAMPLES,
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

/// Reconstruct one 16×16 luma macroblock's 4×4-intra sub-blocks
/// end-to-end from placed coefficient grids, applying the
/// **SVQ3-specific intra-luma DC scale** to each sub-block's inline DC
/// coefficient.
///
/// Identical to [`reconstruct_intra_luma_macroblock_from_coeffs`] except
/// the per-sub-block residual interleave runs
/// [`crate::svq3_dequant::dequantize_transform_intra_luma_block`] instead
/// of the general [`crate::svq3_dequant::dequantize_transform_luma_block`].
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
/// transform and dequantization", an intra luma block whose DC is
/// carried inline uses `dc = 13 · 13 · 1538 · block[0]` for the DC term
/// (the additive override in the dequant formula) rather than running
/// `block[0]` through the general `coeff · svq3_dequant_coeff[Q]` AC
/// scale. This is the correct path for a 4×4-intra macroblock that does
/// **not** carry its luma DCs in a separate block (MB types `1..=24`);
/// the separate-DC-block branch (MB types `0` / `25`) requires the
/// separate luma-DC block transform + distribution, which is not pinned
/// under `docs/video/svq3/` and remains a deferred docs gap.
///
/// `modes` / `coeff_blocks` / `q` follow
/// [`reconstruct_intra_luma_macroblock_from_coeffs`].
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
pub fn reconstruct_intra_luma_macroblock_from_coeffs_intra_dc(
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

        // SVQ3 intra-luma DC scale (13·13·1538·block[0]) for the DC term,
        // general AC dequant + two-sided transform for the rest.
        let residual = dequantize_transform_intra_luma_block(q, coeff_blocks[index]);

        let nb = mb.neighbours_at(bx, by);
        let predicted = predict_intra_4x4(modes[index], nb);
        let recon = reconstruct_4x4(predicted, residual);
        mb.write_block(bx, by, recon);
    }
}

/// Convert an [`crate::svq3_mb::Intra4x4ModeGrid`] (the block-index-ordered
/// `u8` modes the intra-mode VLC decode produces) into the
/// `[Svq3IntraMode; MB_LUMA_BLOCKS]` array the reconstruction loops
/// consume.
///
/// The grid's modes are already validated to lie in `0..=4` by
/// [`crate::svq3_mb::decode_intra_4x4_modes`] (every value is the result
/// of an `INTRA_PRED_TABLE` lookup whose non-sentinel entries are in
/// `0..=4`), so [`Svq3IntraMode::from_value`] cannot fail here; the
/// `Result` is propagated only as a defensive guard.
pub fn intra_modes_from_grid(
    grid: &crate::svq3_mb::Intra4x4ModeGrid,
) -> crate::Result<[Svq3IntraMode; MB_LUMA_BLOCKS]> {
    let mut out = [Svq3IntraMode::DEFAULT; MB_LUMA_BLOCKS];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = Svq3IntraMode::from_value(grid.modes()[i])?;
    }
    Ok(out)
}

/// Decode the 16 4×4-intra prediction modes of one macroblock directly
/// from the slice bitstream, then reconstruct the 16×16 luma plane
/// end-to-end from placed coefficient grids.
///
/// This is the **bitstream-driven** intra-4×4 luma entry point: it
/// composes the intra-mode VLC wire decode
/// ([`crate::svq3_mb::decode_intra_4x4_modes`], spec/01 Gap 3 binding +
/// the wiki §"Intra macroblock information decoding" Golomb-indexed pair
/// VLC) with the per-block residual interleave + predictor-selection +
/// writeback loop of [`reconstruct_intra_luma_macroblock_from_coeffs`].
/// Where that function takes the resolved modes as an argument, this one
/// reads them from the slice bits first.
///
/// `top_avail` / `left_avail` are passed through to the mode decode to
/// govern the out-of-macroblock edge-neighbour availability (whether a
/// neighbour macroblock exists above / to the left in the slice).
/// `coeff_blocks[index]` / `q` are exactly as in
/// [`reconstruct_intra_luma_macroblock_from_coeffs`]. On success
/// `mb.samples` holds the reconstructed 16×16 luma plane and the decoded
/// [`crate::svq3_mb::Intra4x4ModeGrid`] is returned so the caller can
/// thread the per-block modes into the neighbouring macroblocks' intra
/// prediction.
///
/// Propagates the mode-decode errors (`Truncated`, `InvalidFrameCode`,
/// `InvalidIntraPrediction`).
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN` (same contract as
/// [`reconstruct_intra_luma_macroblock_from_coeffs`]).
pub fn decode_and_reconstruct_intra_luma_macroblock(
    br: &mut crate::bitreader::BitReader<'_>,
    mb: &mut LumaMacroblock,
    coeff_blocks: &[[i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS],
    q: u32,
    top_avail: bool,
    left_avail: bool,
) -> crate::Result<crate::svq3_mb::Intra4x4ModeGrid> {
    let grid = crate::svq3_mb::decode_intra_4x4_modes(br, top_avail, left_avail)?;
    let modes = intra_modes_from_grid(&grid)?;
    reconstruct_intra_luma_macroblock_from_coeffs(mb, &modes, coeff_blocks, q);
    Ok(grid)
}

/// The macroblock-wide intra-16×16 luma predictor selection.
///
/// Per `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 4 the
/// SVQ3 16×16 luma intra path uses **one** predictor for the whole
/// macroblock (there is no per-4×4 mode selection — that is the distinct
/// 4×4-intra path driven by [`reconstruct_intra_luma_macroblock_from_coeffs`]).
/// Gap 4 pins the SVQ3 16×16 **plane** predictor (the H.264 plane "but
/// transposed"); the standard H.264 16×16 **DC** predictor is the
/// edge-macroblock fallback used when the plane predictor's neighbours
/// are not both available.
///
/// This enum is the resolved predictor choice the caller supplies; the
/// wire decode that *selects* it (the 16×16 intra-mode signalling inside
/// the predefined-CBP MB-type code) is a deferred docs gap, so the
/// reconstruction entry point takes the resolved mode directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq3Luma16x16Mode {
    /// The standard 16×16 vertical predictor
    /// ([`crate::svq3_pred::predict_vertical_16x16`]): the above row
    /// repeated down every row. Requires the above row.
    Vertical,
    /// The standard 16×16 horizontal predictor
    /// ([`crate::svq3_pred::predict_horizontal_16x16`]): the left
    /// column repeated across every column. Requires the left column.
    Horizontal,
    /// The SVQ3 transposed-plane predictor
    /// ([`crate::svq3_pred::predict_plane_16x16`], Gap 4). Requires both
    /// the above row and left column to be available.
    Plane,
    /// The H.264 16×16 DC predictor
    /// ([`crate::svq3_pred::predict_dc_16x16`]) — the availability-driven
    /// average used at macroblock edges where the plane predictor's
    /// neighbours are not both present.
    Dc,
}

impl Svq3Luma16x16Mode {
    /// Resolve an intra 16×16 `pred_mode` selector (`0..=3`, from
    /// [`crate::svq3_mb::Intra16x16Params`]) to a predictor, falling
    /// back to DC when a required neighbour row/column is unavailable.
    ///
    /// The numbering follows the standard H.264 16×16 mode order
    /// (0 = vertical, 1 = horizontal, 2 = DC, 3 = plane); the wiki
    /// snapshot says SVQ3 intra prediction "is the same as in H.264"
    /// apart from its enumerated quirks, but the binding of the four
    /// selector values to the four predictors is **not pinned** by the
    /// staged docs (`docs/video/svq3/provenance/05` "What was NOT
    /// established") — this resolver carries the standard-numbering
    /// reading.
    #[must_use]
    pub const fn from_pred_mode(mode: u8, top_available: bool, left_available: bool) -> Self {
        match mode {
            0 if top_available => Self::Vertical,
            1 if left_available => Self::Horizontal,
            3 if top_available && left_available => Self::Plane,
            _ => Self::Dc,
        }
    }
}

impl LumaMacroblock {
    /// Build the macroblock-wide 16×16 luma prediction plane for the
    /// supplied [`Svq3Luma16x16Mode`].
    ///
    /// The plane predictor reads the full 16-sample above row + left
    /// column ([`Self::above`] / [`Self::leftcol`]); the DC predictor
    /// reads the same rows but with the availability-driven averaging.
    /// The returned plane is row-major (`pred[y * 16 + x]`).
    #[must_use]
    fn predict_16x16(&self, mode: Svq3Luma16x16Mode) -> [u8; MB_LUMA_DIM * MB_LUMA_DIM] {
        match mode {
            Svq3Luma16x16Mode::Vertical => predict_vertical_16x16(self.above),
            Svq3Luma16x16Mode::Horizontal => predict_horizontal_16x16(self.leftcol),
            Svq3Luma16x16Mode::Plane => predict_plane_16x16(self.above, self.leftcol),
            Svq3Luma16x16Mode::Dc => predict_dc_16x16(
                self.above,
                self.leftcol,
                self.above_available,
                self.left_available,
            ),
        }
    }
}

/// Reconstruct one 16×16 **intra-16×16** luma macroblock end-to-end from
/// placed coefficient grids.
///
/// This is the 16×16-intra counterpart to
/// [`reconstruct_intra_luma_macroblock_from_coeffs`]. The SVQ3 16×16
/// intra path differs structurally from the 4×4-intra path
/// (`docs/video/svq3/spec/01-reconstruction-composition.md` Gap 4):
///
/// 1. **One predictor for the whole macroblock.** The supplied
///    [`Svq3Luma16x16Mode`] produces a single 16×16 prediction plane
///    (transposed-plane or DC), rather than 16 independently-predicted
///    4×4 sub-blocks. No neighbour sequencing within the macroblock is
///    needed — the predictor reads only the out-of-MB above row + left
///    column.
/// 2. **Residual added per 4×4 sub-block in raster order.** Each of the
///    16 luma 4×4 sub-blocks still carries its own residual coefficient
///    grid; each grid is run through the Gap 2 residual interleave
///    ([`crate::svq3_dequant::dequantize_transform_luma_block`]) at the
///    slice quantiser `q` and added onto the matching 4×4 region of the
///    prediction plane with the Gap 5 saturating
///    `Clip1(pred + residual)` writeback.
///
/// `coeff_blocks[index]` is the row-major placed coefficient grid for
/// luma 4×4 sub-block `index`; sub-blocks are indexed in **raster**
/// order (`index = grid_row * 4 + grid_col`, pixel origin
/// `(grid_col*4, grid_row*4)`) — the natural 16×16 raster, distinct from
/// the 4×4-intra path's wiki scan order, because the 16×16 predictor has
/// no intra-mode dependency to sequence. `q` is the slice quantiser; it
/// must satisfy `q < DEQUANT_COEFF_TABLE_LEN`. On return `mb.samples`
/// holds the fully reconstructed 16×16 luma plane.
///
/// Like its 4×4 counterpart this covers the no-separate-DC case
/// (`dc = 0`); the separate-DC-block branch layers on top of
/// [`crate::svq3_dequant::dequantize_transform_luma_block_with_dc`] once
/// the separate-DC presence is decoded from the (still-deferred) MB-type
/// wire format.
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
pub fn reconstruct_intra_16x16_luma_macroblock_from_coeffs(
    mb: &mut LumaMacroblock,
    mode: Svq3Luma16x16Mode,
    coeff_blocks: &[[i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS],
    q: u32,
) {
    // One macroblock-wide prediction plane (Gap 4).
    let plane = mb.predict_16x16(mode);

    // Add each 4×4 sub-block's residual onto the matching plane region,
    // in raster order (no intra-mode sequencing for the 16×16 path).
    for (index, coeff_block) in coeff_blocks.iter().enumerate() {
        let gr = index / MB_GRID_DIM;
        let gc = index % MB_GRID_DIM;
        let by = gr * PRED_4X4_DIM;
        let bx = gc * PRED_4X4_DIM;

        // Spec/01 Gap 2 residual interleave.
        let residual = dequantize_transform_luma_block(q, *coeff_block);

        // Gap 5 saturating writeback over the predicted plane.
        for r in 0..PRED_4X4_DIM {
            for c in 0..PRED_4X4_DIM {
                let px = bx + c;
                let py = by + r;
                let pred = plane[py * PRED_16X16_DIM + px];
                let recon = reconstruct_sample(pred, residual[r * PRED_4X4_DIM + c]);
                mb.samples[py * MB_LUMA_DIM + px] = recon;
            }
        }
    }
}

/// Side length of one 8×8 chroma plane (Cb or Cr) of a macroblock.
pub const CHROMA_PLANE_DIM: usize = PRED_CHROMA_DIM;

/// Side length of the chroma plane measured in 4×4 sub-blocks
/// (`8 / 4 = 2`).
pub const CHROMA_GRID_DIM: usize = CHROMA_PLANE_DIM / PRED_4X4_DIM;

/// Number of chroma AC 4×4 sub-blocks in one 8×8 chroma plane
/// (`2 × 2 = 4`).
pub const CHROMA_PLANE_BLOCKS: usize = CHROMA_GRID_DIM * CHROMA_GRID_DIM;

/// One 8×8 chroma plane (Cb or Cr) being reconstructed, plus the
/// out-of-plane neighbour samples the DC predictor reads.
///
/// The plane is stored row-major (`samples[y * 8 + x]`). SVQ3 forces
/// chroma to **DC-only** prediction
/// (`docs/video/svq3/spec/01-reconstruction-composition.md` Gap 4 — "SVQ3
/// forces chroma to DC mode only"), so the only neighbours read are the
/// 8-sample above row + left column (no corner / plane fit). Their
/// availability mirrors whether a neighbour macroblock exists in the
/// slice, exactly as for [`LumaMacroblock`].
#[derive(Debug, Clone)]
pub struct ChromaPlane {
    /// Reconstructed chroma samples, row-major, `8 × 8`.
    pub samples: [u8; PRED_CHROMA_SAMPLES],
    /// The 8 reconstructed samples directly above the plane
    /// (`above[x]` = pixel at `(x, -1)`). Read only when
    /// [`Self::above_available`] is set.
    pub above: [u8; CHROMA_PLANE_DIM],
    /// The 8 reconstructed samples directly to the left of the plane
    /// (`leftcol[y]` = pixel at `(-1, y)`). Read only when
    /// [`Self::left_available`] is set.
    pub leftcol: [u8; CHROMA_PLANE_DIM],
    /// Whether a neighbour macroblock exists above this plane.
    pub above_available: bool,
    /// Whether a neighbour macroblock exists to the left of this plane.
    pub left_available: bool,
}

impl ChromaPlane {
    /// A chroma plane with all samples / neighbours zeroed and both
    /// neighbours marked unavailable (a top-left macroblock's plane).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            samples: [0u8; PRED_CHROMA_SAMPLES],
            above: [0u8; CHROMA_PLANE_DIM],
            leftcol: [0u8; CHROMA_PLANE_DIM],
            above_available: false,
            left_available: false,
        }
    }

    /// Read the reconstructed sample at plane-relative pixel `(x, y)`
    /// (both `0..=7`).
    #[inline]
    #[must_use]
    pub const fn sample(&self, x: usize, y: usize) -> u8 {
        self.samples[y * CHROMA_PLANE_DIM + x]
    }
}

impl Default for ChromaPlane {
    fn default() -> Self {
        Self::new()
    }
}

/// Reconstruct one 8×8 chroma plane (Cb or Cr) of an intra macroblock,
/// end-to-end from the 2×2 chroma DC block plus the four chroma AC 4×4
/// coefficient grids.
///
/// SVQ3 chroma reconstruction composes the staged facts of
/// `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 4 and
/// `docs/video/svq3/spec/04-dc-secondary-transform.md` §2/§3:
///
/// 1. **DC-only prediction (Gap 4).** The whole 8×8 plane is predicted by
///    the chroma DC predictor ([`crate::svq3_pred::predict_chroma_dc_8x8`]),
///    averaging the available 8-sample above row + left column per the
///    four 4×4 quadrants. There is no plane / vertical / horizontal
///    chroma mode in SVQ3.
/// 2. **Separate 2×2 chroma DC block (spec/04 §2).** The four decoded
///    chroma DC levels (`dc_block`, row-major 2×2 coded order) are
///    dequantised with the **chroma quantiser index** (spec/04 §3) and
///    run through the 2×2 Hadamard-and-halve secondary transform
///    ([`crate::svq3_dequant::dequantize_chroma_dc_levels`]), yielding
///    `B_k` for the four 4×4 chroma quadrants in raster order (§2.3);
///    `B_k` is coefficient position 0 of quadrant `k`, i.e. the additive
///    term `169 · B_k` in the fused store.
/// 3. **Chroma AC residual interleave (Gap 2 + spec/04 §3).** Each
///    chroma 4×4 quadrant's AC level grid (`ac_blocks[index]`, raster
///    `index = qr*2 + qc`) is run through the same dequant·scale →
///    two-sided `M·X·Mᵀ` transform → fused `+ dc + 0x80000 >> 20` store
///    as luma, with the ladder indexed through the chroma remap and the
///    per-block `dc` override set to `169 · B_k`
///    ([`crate::svq3_dequant::dequantize_transform_luma_block_with_dc`]).
///
/// The Gap 5 saturating `Clip1(pred + residual)` writeback then composes
/// the predicted plane with each quadrant's residual. On return
/// `plane.samples` holds the fully reconstructed 8×8 chroma plane.
///
/// `q` is the slice quantiser; it must satisfy `q < DEQUANT_COEFF_TABLE_LEN`.
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
pub fn reconstruct_intra_chroma_plane_from_coeffs(
    plane: &mut ChromaPlane,
    dc_block: [i32; CHROMA_PLANE_BLOCKS],
    ac_blocks: &[[i32; PRED_4X4_SAMPLES]; CHROMA_PLANE_BLOCKS],
    q: u32,
) {
    // Gap 4: DC-only prediction over the whole 8×8 plane.
    let predicted = predict_chroma_dc_8x8(
        plane.above,
        plane.leftcol,
        plane.above_available,
        plane.left_available,
    );

    // spec/04 §2.1 steps 2–4: dequantise the four chroma DC levels with
    // the chroma quantiser index, apply the Hadamard-and-halve secondary
    // transform, and hold the four raster-order per-quadrant DC terms.
    // B_k enters the fused store as the additive 169·B_k (equivalent to
    // coefficient position 0 of the quadrant's block).
    let b = dequantize_chroma_dc_levels(q, dc_block);

    // spec/04 §3: chroma AC coefficients index the ladder through the
    // chroma quantiser remap.
    let chroma_q = chroma_quantiser_index(q);

    // For each chroma 4×4 quadrant (raster index qr*2 + qc): interleave
    // the AC residual with the quadrant's chroma DC override, then add
    // onto the predicted plane with the Gap 5 saturating writeback.
    for (index, ac_block) in ac_blocks.iter().enumerate() {
        let qr = index / CHROMA_GRID_DIM;
        let qc = index % CHROMA_GRID_DIM;
        let by = qr * PRED_4X4_DIM;
        let bx = qc * PRED_4X4_DIM;

        let residual =
            dequantize_transform_luma_block_with_dc(chroma_q, *ac_block, 169 * b[index] as i64);

        for r in 0..PRED_4X4_DIM {
            for c in 0..PRED_4X4_DIM {
                let px = bx + c;
                let py = by + r;
                let pred = predicted[py * CHROMA_PLANE_DIM + px];
                let recon = reconstruct_sample(pred, residual[r * PRED_4X4_DIM + c]);
                plane.samples[py * CHROMA_PLANE_DIM + px] = recon;
            }
        }
    }
}

/// The luma intra-prediction regime for a whole intra macroblock.
///
/// Per `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock layer"
/// an intra macroblock's luma plane is reconstructed either as 16
/// independently-mode-predicted 4×4 sub-blocks (the 4×4-intra regime) or
/// as a single 16×16 predictor (the 16×16-intra regime). The two regimes
/// drive different reconstruction loops
/// ([`reconstruct_intra_luma_macroblock_from_coeffs`] vs
/// [`reconstruct_intra_16x16_luma_macroblock_from_coeffs`]); this enum
/// carries the already-resolved choice plus its per-regime parameters.
///
/// The wire decode that *selects* the regime (the MB-type Golomb code +
/// its predefined-CBP / intra-mode-pair sub-streams) is a deferred docs
/// gap — the wiki names the MB-type codes but the CBP code-number
/// mapping is "the same way as in H.264" without the table being staged.
/// So this enum is supplied by the caller with the resolved modes /
/// coefficient grids already in hand.
#[derive(Debug, Clone)]
pub enum Svq3LumaIntra {
    /// 4×4-intra luma: 16 per-sub-block intra modes (indexed by raster
    /// block index) + 16 placed coefficient grids.
    Blocks4x4 {
        /// Per-sub-block intra modes, indexed by raster block index.
        modes: [Svq3IntraMode; MB_LUMA_BLOCKS],
        /// Per-sub-block placed coefficient grids.
        coeff_blocks: [[i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS],
    },
    /// 16×16-intra luma: one macroblock-wide predictor + 16 placed
    /// coefficient grids (added in raster order).
    Whole16x16 {
        /// The macroblock-wide 16×16 predictor choice.
        mode: Svq3Luma16x16Mode,
        /// Per-sub-block placed coefficient grids (raster order).
        coeff_blocks: [[i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS],
    },
}

/// One fully-reconstructed intra macroblock: the 16×16 luma plane plus
/// the two 8×8 chroma planes (Cb, Cr).
///
/// This is the assembly unit a frame walk emits per intra macroblock. It
/// composes the three per-plane reconstruction paths this module owns
/// (the 4×4-intra / 16×16-intra luma loop and the chroma 8×8 plane loop)
/// behind a single carrier so the (deferred) frame walk can drive one MB
/// at a time and read back all three reconstructed planes.
#[derive(Debug, Clone)]
pub struct Svq3IntraMacroblock {
    /// The 16×16 luma plane (+ its out-of-MB neighbours).
    pub luma: LumaMacroblock,
    /// The 8×8 Cb chroma plane (+ its out-of-MB neighbours).
    pub cb: ChromaPlane,
    /// The 8×8 Cr chroma plane (+ its out-of-MB neighbours).
    pub cr: ChromaPlane,
}

impl Svq3IntraMacroblock {
    /// A macroblock with all three planes zeroed and every out-of-MB
    /// neighbour marked unavailable (a top-left macroblock).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            luma: LumaMacroblock::new(),
            cb: ChromaPlane::new(),
            cr: ChromaPlane::new(),
        }
    }
}

impl Default for Svq3IntraMacroblock {
    fn default() -> Self {
        Self::new()
    }
}

/// The placed coefficient inputs for one chroma plane: the 2×2 chroma DC
/// block plus the four chroma AC 4×4 grids.
///
/// Grouped so [`reconstruct_intra_macroblock`] can take both chroma
/// planes' coefficients without a six-argument signature.
#[derive(Debug, Clone)]
pub struct ChromaPlaneCoeffs {
    /// The placed 2×2 chroma DC block (row-major, 4 entries).
    pub dc_block: [i32; CHROMA_PLANE_BLOCKS],
    /// The four chroma AC 4×4 placed coefficient grids (raster
    /// quadrant order).
    pub ac_blocks: [[i32; PRED_4X4_SAMPLES]; CHROMA_PLANE_BLOCKS],
}

/// Reconstruct one whole intra macroblock — all three planes — from its
/// resolved per-plane intra modes + placed coefficient grids.
///
/// Composes the three per-plane reconstruction entry points this module
/// owns into the single-macroblock unit a frame walk consumes:
///
/// * **Luma** — dispatched on `luma` ([`Svq3LumaIntra`]): the 4×4-intra
///   per-sub-block mode loop
///   ([`reconstruct_intra_luma_macroblock_from_coeffs`]) or the
///   16×16-intra whole-macroblock loop
///   ([`reconstruct_intra_16x16_luma_macroblock_from_coeffs`]).
/// * **Cb / Cr chroma** — each via
///   [`reconstruct_intra_chroma_plane_from_coeffs`] (DC-only prediction +
///   2×2 chroma DC + chroma AC interleave).
///
/// All three planes share the slice quantiser `q`. The macroblock's
/// out-of-MB neighbour rows/columns (above / left / corner +
/// availability flags) must already be populated on `mb.luma` / `mb.cb` /
/// `mb.cr` before this call (the frame walk fills them from the
/// previously-reconstructed neighbour macroblocks). On return all three
/// planes' `samples` hold the fully reconstructed pixels.
///
/// This is the deepest composition spec/01 pins end-to-end for an intra
/// macroblock; the wire decode that *produces* the modes / coefficient
/// grids / quantiser (intra-mode VLC, CBP, MB-type Golomb) remains a
/// deferred docs gap, so the inputs are supplied directly.
///
/// # Panics
///
/// Panics if `q >= DEQUANT_COEFF_TABLE_LEN`.
pub fn reconstruct_intra_macroblock(
    mb: &mut Svq3IntraMacroblock,
    luma: &Svq3LumaIntra,
    cb: &ChromaPlaneCoeffs,
    cr: &ChromaPlaneCoeffs,
    q: u32,
) {
    match luma {
        Svq3LumaIntra::Blocks4x4 {
            modes,
            coeff_blocks,
        } => reconstruct_intra_luma_macroblock_from_coeffs(&mut mb.luma, modes, coeff_blocks, q),
        Svq3LumaIntra::Whole16x16 { mode, coeff_blocks } => {
            reconstruct_intra_16x16_luma_macroblock_from_coeffs(
                &mut mb.luma,
                *mode,
                coeff_blocks,
                q,
            )
        }
    }
    reconstruct_intra_chroma_plane_from_coeffs(&mut mb.cb, cb.dc_block, &cb.ac_blocks, q);
    reconstruct_intra_chroma_plane_from_coeffs(&mut mb.cr, cr.dc_block, &cr.ac_blocks, q);
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

    // ---- Intra-16×16 luma macroblock reconstruction -------------------

    #[test]
    fn intra_16x16_dc_no_neighbours_zero_residual_is_flat_128() {
        // A top-left 16×16-intra MB with no neighbours, DC mode, zero
        // residual: the whole plane is flat 128.
        let mut mb = LumaMacroblock::new();
        let coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];
        reconstruct_intra_16x16_luma_macroblock_from_coeffs(
            &mut mb,
            Svq3Luma16x16Mode::Dc,
            &coeffs,
            12,
        );
        assert!(mb.samples.iter().all(|&s| s == 128), "expected flat 128");
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn intra_16x16_dc_with_both_neighbours_averages() {
        // Both neighbours available, all samples = 100 → DC = average =
        // 100; zero residual ⇒ flat 100.
        let mut mb = LumaMacroblock::new();
        mb.above = [100; 16];
        mb.leftcol = [100; 16];
        mb.above_available = true;
        mb.left_available = true;
        let coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];
        reconstruct_intra_16x16_luma_macroblock_from_coeffs(
            &mut mb,
            Svq3Luma16x16Mode::Dc,
            &coeffs,
            12,
        );
        assert!(mb.samples.iter().all(|&s| s == 100), "expected flat 100");
    }

    #[test]
    fn intra_16x16_plane_flat_neighbours_is_flat() {
        // Plane prediction over flat neighbours (all 80, corner irrelevant
        // to the plane formula): H = V = 0, a = 16*(80+80) = 2560,
        // pred = (2560 + 16) >> 5 = 80 everywhere. Zero residual ⇒ flat 80.
        let mut mb = LumaMacroblock::new();
        mb.above = [80; 16];
        mb.leftcol = [80; 16];
        mb.above_available = true;
        mb.left_available = true;
        let coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];
        reconstruct_intra_16x16_luma_macroblock_from_coeffs(
            &mut mb,
            Svq3Luma16x16Mode::Plane,
            &coeffs,
            12,
        );
        assert!(mb.samples.iter().all(|&s| s == 80), "expected flat 80");
    }

    #[test]
    fn intra_16x16_residual_in_block_zero_lifts_only_that_region() {
        // DC MB (flat 128) with a single residual coefficient in raster
        // sub-block 0 (pixel region (0..4, 0..4)). The residual lifts
        // that footprint; the rest stays flat 128.
        let mut mb = LumaMacroblock::new();
        let mut coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];
        coeffs[0][0] = 1; // pure DC coefficient in sub-block 0.
        let q = 14;
        reconstruct_intra_16x16_luma_macroblock_from_coeffs(
            &mut mb,
            Svq3Luma16x16Mode::Dc,
            &coeffs,
            q,
        );
        let residual = dequantize_transform_luma_block(q, coeffs[0]);
        let expected = (128 + residual[0]).clamp(0, 255) as u8;
        assert_ne!(expected, 128, "pure-DC residual must move the plane");
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(mb.sample(x, y), expected, "block-0 ({x},{y})");
            }
        }
        // A pixel outside block 0's footprint is untouched.
        assert_eq!(mb.sample(8, 8), 128);
    }

    #[test]
    fn intra_16x16_residual_maps_to_raster_subblock_position() {
        // Raster sub-block index 5 is at grid (1, 1) → pixel origin
        // (4, 4). A DC coefficient there must lift the (4..8, 4..8)
        // region, confirming the raster index → pixel-origin mapping.
        let mut mb = LumaMacroblock::new();
        let mut coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];
        coeffs[5][0] = 2;
        let q = 10;
        reconstruct_intra_16x16_luma_macroblock_from_coeffs(
            &mut mb,
            Svq3Luma16x16Mode::Dc,
            &coeffs,
            q,
        );
        let residual = dequantize_transform_luma_block(q, coeffs[5]);
        let expected = (128 + residual[0]).clamp(0, 255) as u8;
        for y in 4..8 {
            for x in 4..8 {
                assert_eq!(mb.sample(x, y), expected, "block-5 ({x},{y})");
            }
        }
        assert_eq!(mb.sample(0, 0), 128, "block-0 untouched");
    }

    // ---- Intra chroma 8×8 plane reconstruction ------------------------

    #[test]
    fn chroma_constants() {
        assert_eq!(CHROMA_PLANE_DIM, 8);
        assert_eq!(CHROMA_GRID_DIM, 2);
        assert_eq!(CHROMA_PLANE_BLOCKS, 4);
    }

    #[test]
    fn chroma_no_neighbours_zero_residual_is_flat_128() {
        // Top-left plane: DC predicts 128 everywhere; zero DC block + zero
        // AC ⇒ flat 128.
        let mut plane = ChromaPlane::new();
        let dc_block = [0i32; CHROMA_PLANE_BLOCKS];
        let ac = [[0i32; 16]; CHROMA_PLANE_BLOCKS];
        reconstruct_intra_chroma_plane_from_coeffs(&mut plane, dc_block, &ac, 12);
        assert!(plane.samples.iter().all(|&s| s == 128), "expected flat 128");
    }

    #[test]
    fn chroma_both_neighbours_dc_averages() {
        // Both neighbours = 90 everywhere → each quadrant DC =
        // (Σ4top + Σ4left + 4) >> 3 = (360 + 360 + 4) >> 3 = 90. Zero
        // residual ⇒ flat 90.
        let mut plane = ChromaPlane::new();
        plane.above = [90; 8];
        plane.leftcol = [90; 8];
        plane.above_available = true;
        plane.left_available = true;
        let dc_block = [0i32; CHROMA_PLANE_BLOCKS];
        let ac = [[0i32; 16]; CHROMA_PLANE_BLOCKS];
        reconstruct_intra_chroma_plane_from_coeffs(&mut plane, dc_block, &ac, 12);
        assert!(plane.samples.iter().all(|&s| s == 90), "expected flat 90");
    }

    #[test]
    fn chroma_dc_c0_level_spreads_uniformly_over_all_quadrants() {
        // spec/04 §2.2/§2.3: a single level at chroma DC position c0
        // dequantises to dc0, and the Hadamard-and-halve butterfly
        // spreads B_k = dc0/2 to ALL four quadrants — each quadrant's
        // 4×4 footprint lifts by the same fused-store amount.
        let mut plane = ChromaPlane::new();
        let mut dc_block = [0i32; CHROMA_PLANE_BLOCKS];
        dc_block[0] = 2;
        let ac = [[0i32; 16]; CHROMA_PLANE_BLOCKS];
        let q = 14;
        reconstruct_intra_chroma_plane_from_coeffs(&mut plane, dc_block, &ac, q);

        let b = dequantize_chroma_dc_levels(q, dc_block);
        assert_eq!(b[0], b[1]);
        assert_eq!(b[1], b[2]);
        assert_eq!(b[2], b[3]);
        let residual = dequantize_transform_luma_block_with_dc(
            chroma_quantiser_index(q),
            [0i32; 16],
            169 * b[0] as i64,
        );
        let expected = (128 + residual[0]).clamp(0, 255) as u8;
        assert_ne!(expected, 128, "fixture must produce a visible lift");
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(plane.sample(x, y), expected, "({x},{y})");
            }
        }
    }

    #[test]
    fn chroma_dc_c3_level_signs_quadrants_by_butterfly() {
        // A single level at chroma DC position c3 produces
        // B = (+, −, −, +)·dc3/2 across the raster quadrants; each
        // quadrant's footprint lifts by its own signed amount.
        let mut plane = ChromaPlane::new();
        let mut dc_block = [0i32; CHROMA_PLANE_BLOCKS];
        dc_block[3] = 3;
        let ac = [[0i32; 16]; CHROMA_PLANE_BLOCKS];
        let q = 10;
        reconstruct_intra_chroma_plane_from_coeffs(&mut plane, dc_block, &ac, q);

        let b = dequantize_chroma_dc_levels(q, dc_block);
        assert!(b[0] > 0 && b[1] < 0 && b[2] < 0 && b[3] > 0, "{b:?}");
        for (index, bk) in b.iter().enumerate() {
            let residual = dequantize_transform_luma_block_with_dc(
                chroma_quantiser_index(q),
                [0i32; 16],
                169 * *bk as i64,
            );
            let expected = (128 + residual[0]).clamp(0, 255) as u8;
            let (qr, qc) = (index / 2, index % 2);
            for y in 0..4 {
                for x in 0..4 {
                    assert_eq!(
                        plane.sample(qc * 4 + x, qr * 4 + y),
                        expected,
                        "quadrant {index} ({x},{y})"
                    );
                }
            }
        }
    }

    #[test]
    fn chroma_ac_residual_composes_with_prediction() {
        // An AC coefficient in quadrant 0 produces a non-flat residual
        // over that quadrant; verify the reconstruction differs from the
        // bare flat DC prediction inside quadrant 0 and the result is a
        // valid clamped plane.
        let mut plane = ChromaPlane::new();
        let dc_block = [0i32; CHROMA_PLANE_BLOCKS];
        let mut ac = [[0i32; 16]; CHROMA_PLANE_BLOCKS];
        ac[0][5] = 4; // a non-DC AC coefficient in quadrant 0.
        let q = 9;
        reconstruct_intra_chroma_plane_from_coeffs(&mut plane, dc_block, &ac, q);

        let residual = dequantize_transform_luma_block_with_dc(chroma_quantiser_index(q), ac[0], 0);
        let mut differs = false;
        for r in 0..4 {
            for c in 0..4 {
                let expected = (128 + residual[r * 4 + c]).clamp(0, 255) as u8;
                assert_eq!(plane.sample(c, r), expected, "quadrant-0 ({c},{r})");
                if expected != 128 {
                    differs = true;
                }
            }
        }
        assert!(differs, "AC residual must move at least one sample");
    }

    // ---- Whole intra macroblock composition ---------------------------

    #[test]
    fn whole_mb_4x4_all_dc_no_neighbours_is_flat_128() {
        // A top-left intra MB, luma all-DC 4×4, chroma DC-only, every
        // coefficient zero ⇒ all three planes flat 128.
        let mut mb = Svq3IntraMacroblock::new();
        let luma = Svq3LumaIntra::Blocks4x4 {
            modes: [Svq3IntraMode::Dc; MB_LUMA_BLOCKS],
            coeff_blocks: [[0i32; 16]; MB_LUMA_BLOCKS],
        };
        let chroma = ChromaPlaneCoeffs {
            dc_block: [0i32; CHROMA_PLANE_BLOCKS],
            ac_blocks: [[0i32; 16]; CHROMA_PLANE_BLOCKS],
        };
        reconstruct_intra_macroblock(&mut mb, &luma, &chroma, &chroma, 12);
        assert!(mb.luma.samples.iter().all(|&s| s == 128), "luma flat 128");
        assert!(mb.cb.samples.iter().all(|&s| s == 128), "cb flat 128");
        assert!(mb.cr.samples.iter().all(|&s| s == 128), "cr flat 128");
    }

    #[test]
    fn whole_mb_16x16_matches_standalone_luma_path() {
        // The dispatched 16×16 luma reconstruction must equal the direct
        // 16×16 entry point on the same inputs.
        let mut coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];
        coeffs[3][0] = 2;
        coeffs[10][1] = -1;
        let q = 11;

        let mut mb = Svq3IntraMacroblock::new();
        mb.luma.above = [70; 16];
        mb.luma.leftcol = [70; 16];
        mb.luma.above_available = true;
        mb.luma.left_available = true;
        let luma = Svq3LumaIntra::Whole16x16 {
            mode: Svq3Luma16x16Mode::Plane,
            coeff_blocks: coeffs,
        };
        let chroma = ChromaPlaneCoeffs {
            dc_block: [0i32; CHROMA_PLANE_BLOCKS],
            ac_blocks: [[0i32; 16]; CHROMA_PLANE_BLOCKS],
        };
        reconstruct_intra_macroblock(&mut mb, &luma, &chroma, &chroma, q);

        let mut direct = LumaMacroblock::new();
        direct.above = [70; 16];
        direct.leftcol = [70; 16];
        direct.above_available = true;
        direct.left_available = true;
        reconstruct_intra_16x16_luma_macroblock_from_coeffs(
            &mut direct,
            Svq3Luma16x16Mode::Plane,
            &coeffs,
            q,
        );
        assert_eq!(mb.luma.samples, direct.samples);
    }

    #[test]
    fn whole_mb_chroma_planes_are_independent() {
        // Distinct Cb / Cr coefficient inputs must produce distinct
        // chroma planes (the dispatcher must not cross-wire them).
        let mut mb = Svq3IntraMacroblock::new();
        let luma = Svq3LumaIntra::Blocks4x4 {
            modes: [Svq3IntraMode::Dc; MB_LUMA_BLOCKS],
            coeff_blocks: [[0i32; 16]; MB_LUMA_BLOCKS],
        };
        let mut cb = ChromaPlaneCoeffs {
            dc_block: [0i32; CHROMA_PLANE_BLOCKS],
            ac_blocks: [[0i32; 16]; CHROMA_PLANE_BLOCKS],
        };
        // Lift Cb quadrant 0 only. The chroma DC residual is tiny in
        // magnitude (the pre-finalise term is heavily scaled down by the
        // >>3 / >>1 / >>20 shifts), so a large coefficient + high
        // quantiser is needed to clear a residual of 1.
        cb.dc_block[0] = 16;
        let cr = ChromaPlaneCoeffs {
            dc_block: [0i32; CHROMA_PLANE_BLOCKS],
            ac_blocks: [[0i32; 16]; CHROMA_PLANE_BLOCKS],
        };
        reconstruct_intra_macroblock(&mut mb, &luma, &cb, &cr, 30);
        // A DC-only 2×2 input spreads through the [[8,8],[8,−8]] Hadamard
        // to all four chroma DC terms equally, so the whole Cb plane is
        // lifted off the flat-128 DC prediction; Cr (all-zero input) stays
        // flat 128. The point under test is the dispatcher keeping the two
        // chroma planes independent.
        assert!(mb.cb.samples.iter().all(|&s| s != 128), "cb plane lifted");
        assert!(mb.cr.samples.iter().all(|&s| s == 128), "cr flat 128");
    }

    /// Pack `(width, value)` items MSB-first into bytes (mirrors the
    /// `svq3_mb` test helper).
    fn pack(items: &[(u32, u32)]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut bit_cursor: usize = 0;
        for &(width, value) in items {
            for i in (0..width).rev() {
                let bit = ((value >> i) & 1) as u8;
                let byte_idx = bit_cursor / 8;
                if byte_idx >= out.len() {
                    out.push(0);
                }
                let shift = 7 - (bit_cursor % 8);
                out[byte_idx] |= bit << shift;
                bit_cursor += 1;
            }
        }
        out
    }

    /// `(width, value)` for the unsigned exp-Golomb code of `n`.
    fn ue(n: u32) -> (u32, u32) {
        let p = n + 1;
        let leading = 31 - p.leading_zeros();
        (2 * leading + 1, p)
    }

    #[test]
    fn intra_modes_from_grid_maps_every_block() {
        // Decode the all-code-0 stream (no neighbour MBs) into a grid,
        // then convert it. Every entry must be a valid Svq3IntraMode and
        // round-trip back to the grid's u8 value.
        let bytes = pack(&[ue(0); 8]);
        let mut br = crate::bitreader::BitReader::new(&bytes);
        let grid = crate::svq3_mb::decode_intra_4x4_modes(&mut br, false, false).unwrap();
        let modes = intra_modes_from_grid(&grid).unwrap();
        for (i, m) in modes.iter().enumerate() {
            assert_eq!(m.value(), grid.modes()[i]);
        }
        // Block 0 decoded to mode 2 (DC) for the all-code-0 corner case.
        assert_eq!(modes[0], Svq3IntraMode::Dc);
    }

    #[test]
    fn decode_and_reconstruct_intra_luma_macroblock_flat_dc() {
        // All-code-0 intra modes (block 0 = DC), zero coefficients. With
        // no neighbours available the DC predictor yields 128 for every
        // sub-block, and a zero residual leaves the plane flat 128.
        let bytes = pack(&[ue(0); 8]);
        let mut br = crate::bitreader::BitReader::new(&bytes);
        let mut mb = LumaMacroblock::new();
        let coeffs = [[0i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS];
        let grid = decode_and_reconstruct_intra_luma_macroblock(
            &mut br, &mut mb, &coeffs, 20, false, false,
        )
        .unwrap();
        // The returned grid's block 0 is DC (mode 2).
        assert_eq!(grid.mode(0), Some(2));
        // Flat-128 reconstruction (DC over unavailable neighbours = 128,
        // zero residual).
        assert!(mb.samples.iter().all(|&s| s == 128), "flat 128 plane");
    }

    #[test]
    fn decode_and_reconstruct_matches_two_step_path() {
        // The end-to-end entry must agree with decode-then-reconstruct
        // run separately, for a non-trivial coefficient block.
        let bytes = pack(&[ue(0); 8]);
        let mut coeffs = [[0i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS];
        coeffs[0][0] = 5;
        coeffs[7][3] = -2;

        // Path A: fused entry.
        let mut br_a = crate::bitreader::BitReader::new(&bytes);
        let mut mb_a = LumaMacroblock::new();
        decode_and_reconstruct_intra_luma_macroblock(
            &mut br_a, &mut mb_a, &coeffs, 18, false, false,
        )
        .unwrap();

        // Path B: decode modes, convert, reconstruct — separately.
        let mut br_b = crate::bitreader::BitReader::new(&bytes);
        let grid = crate::svq3_mb::decode_intra_4x4_modes(&mut br_b, false, false).unwrap();
        let modes = intra_modes_from_grid(&grid).unwrap();
        let mut mb_b = LumaMacroblock::new();
        reconstruct_intra_luma_macroblock_from_coeffs(&mut mb_b, &modes, &coeffs, 18);

        assert_eq!(mb_a.samples, mb_b.samples);
    }

    #[test]
    fn intra_dc_recon_differs_from_general_when_dc_present() {
        // With a non-zero inline DC coefficient the intra-DC recon path
        // (special INTRA_LUMA_DC_SCALE) must differ from the general AC
        // path. Use DC modes over flat-128 neighbours so the only
        // difference is the residual interleave.
        let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];
        let mut coeffs = [[0i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS];
        for c in coeffs.iter_mut() {
            c[0] = 2;
        }

        let mut mb_intra = LumaMacroblock::new();
        reconstruct_intra_luma_macroblock_from_coeffs_intra_dc(&mut mb_intra, &modes, &coeffs, 14);

        let mut mb_general = LumaMacroblock::new();
        reconstruct_intra_luma_macroblock_from_coeffs(&mut mb_general, &modes, &coeffs, 14);

        assert_ne!(mb_intra.samples, mb_general.samples);
    }

    #[test]
    fn intra_dc_recon_matches_general_when_no_dc() {
        // With block[0] == 0 everywhere the intra-DC path reduces to the
        // general path.
        let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];
        let mut coeffs = [[0i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS];
        coeffs[3][6] = 5;

        let mut mb_intra = LumaMacroblock::new();
        reconstruct_intra_luma_macroblock_from_coeffs_intra_dc(&mut mb_intra, &modes, &coeffs, 22);

        let mut mb_general = LumaMacroblock::new();
        reconstruct_intra_luma_macroblock_from_coeffs(&mut mb_general, &modes, &coeffs, 22);

        assert_eq!(mb_intra.samples, mb_general.samples);
    }

    #[test]
    fn intra_dc_recon_flat_dc_zero_coeffs_is_128() {
        // Zero coefficients + DC modes + no neighbours → flat 128 plane
        // (the intra-DC scale of a zero DC coefficient is zero).
        let modes = [Svq3IntraMode::Dc; MB_LUMA_BLOCKS];
        let coeffs = [[0i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS];
        let mut mb = LumaMacroblock::new();
        reconstruct_intra_luma_macroblock_from_coeffs_intra_dc(&mut mb, &modes, &coeffs, 20);
        assert!(mb.samples.iter().all(|&s| s == 128));
    }

    #[test]
    fn decode_and_reconstruct_propagates_truncation() {
        // Only one codeword present; the mode decode needs eight.
        let bytes = pack(&[ue(0)]);
        let mut br = crate::bitreader::BitReader::new(&bytes);
        let mut mb = LumaMacroblock::new();
        let coeffs = [[0i32; PRED_4X4_SAMPLES]; MB_LUMA_BLOCKS];
        let err = decode_and_reconstruct_intra_luma_macroblock(
            &mut br, &mut mb, &coeffs, 20, false, false,
        )
        .unwrap_err();
        assert!(matches!(err, crate::Error::Truncated));
    }
}
