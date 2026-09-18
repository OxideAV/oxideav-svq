//! SVQ3 access-unit decoder — the slice walk and the macroblock layer.
//!
//! Composes the parse + arithmetic layers into whole-picture decodes,
//! following `docs/video/svq3/spec/07-slice-and-macroblock-layer.md`
//! (the envelope, the slice header, the macroblock loop and its
//! termination, the per-slice-type macroblock kinds, the intra 4×4 and
//! intra 16×16 element orders, the quantiser-delta placement and the
//! prediction-mode bindings), `spec/08-inter-macroblock-layer.md` (the
//! P-slice inter macroblocks) and `spec/09-intra-picture-edge-filter.md`
//! (the pass that follows the last macroblock of an I picture).
//!
//! ## Element order (spec/07 §6–§9)
//!
//! * **Intra 4×4** — `mb_type`; eight pair codes (`tables/07`, ranks
//!   resolved through `tables/08`); `coded_block_pattern` through the
//!   intra table; `qp_delta` **only in P and B slices** and only when
//!   the pattern is non-zero and `mb_qp_delta_enable` is set; for each
//!   coded 8×8 quadrant its four 4×4 residual blocks in raster order —
//!   two-list alternate-scan blocks below quantiser 24, single
//!   normal-zigzag lists otherwise; then, if `cbp_chroma ≥ 1`, the
//!   chroma DC block of Cb **then of Cr**, and if `cbp_chroma = 2` the
//!   four chroma AC blocks of Cb then the four of Cr (scan position 1).
//! * **Intra 16×16** — `mb_type` (record → `(pred_mode, cbp_chroma,
//!   luma_ac)`); `qp_delta` **always**; the luma DC block (normal
//!   zigzag, 16 positions); sixteen luma AC lists (scan position 1)
//!   when `luma_ac`; the chroma section as above.
//! * **Flat-128** (P slices, code 33) — luma and chroma predicted as
//!   the constant 128, then the intra 4×4 body from the pattern on.
//! * **Inter** (P slices, codes 0…7) — spec/08.
//!
//! Reconstruction is per block in the same order (a block's prediction
//! reads its neighbours' reconstructed samples); a coefficient is
//! `level × dequant[q]` with no separate intra DC scale (spec/06 §4 —
//! the worked instance there, a lone level +1 at quantiser 13 adding 3
//! to every sample of the block, is pinned by a test).
//!
//! ## Availability
//!
//! Intra 4×4 prediction and the `tables/08` contexts use a per-4×4-block
//! map that is cleared at the start of every slice (spec/07 §10.2), so
//! "available" means *decoded earlier in this slice*; the intra 16×16
//! and chroma DC predictors use picture-edge availability (§10.1,
//! §10.3). The same map is the motion-vector availability of spec/08
//! §4.3.
//!
//! ## Slice termination (spec/07 §4)
//!
//! After every macroblock: if `bit_position + 7 ≥ 8·length` the slice
//! ends when the position is byte-aligned or the remaining bits of the
//! current byte are zero; the loop also ends at the picture's last
//! macroblock.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::svq3::{
    classify_packet_byte, macroblock_position, mb_grid_dims, num_macroblocks, parse_wire_slice,
    read_universal_code, Svq3FrameType, Svq3MacroblockPosition, Svq3PacketKind, Svq3SequenceHeader,
    Svq3SliceHeader,
};
use crate::svq3_cbp::{read_cbp_inter, read_cbp_intra, CodedBlockPattern};
use crate::svq3_coeff::{
    decode_chroma_dc_2x2, decode_residual_4x4_alt, decode_residual_4x4_normal,
};
use crate::svq3_dequant::{
    dequantize_transform_luma_block, luma_dc_secondary_transform, DEQUANT_COEFF_TABLE_LEN,
};
use crate::svq3_mb::{
    classify_mb_type, decode_intra_4x4_modes_with_context, read_inter_mv_precision_p_frame,
    Intra16x16Params, IntraMbKind, PFrameInterMode, Svq3MbType, Svq3MvPrecision,
    INTRA_4X4_BLOCK_RASTER, INTRA_4X4_CONTEXT_OTHER,
};
use crate::svq3_mc::{motion_compensate_block, motion_compensate_chroma_block};
use crate::svq3_mv::{read_mv_difference, read_quantiser_delta};
use crate::svq3_picture::{ChromaSelect, Svq3Picture};
use crate::svq3_pred::{
    predict_chroma_dc_8x8, predict_intra_4x4, reconstruct_4x4, Svq3IntraMode, PRED_4X4_DIM,
    PRED_CHROMA_SAMPLES,
};
use crate::svq3_recon::{
    add_luma_residual_blocks, reconstruct_chroma_plane_with_prediction,
    reconstruct_intra_16x16_luma_macroblock_with_dc, ChromaPlane, LumaMacroblock,
    Svq3Luma16x16Mode,
};
use crate::svq3_scan::{ALT_SCAN_4X4_SCAN, ALT_SCAN_QUANTISER_THRESHOLD, NORMAL_ZIGZAG_4X4_SCAN};

/// Decoder-side options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Svq3DecodeOptions {
    /// Run the spec/09 edge filter over every I picture (the vendor
    /// decoder's behaviour; the filtered picture is displayed and is
    /// the reference for the following P pictures). `false` yields the
    /// unfiltered reconstruction, which is what the fixtures'
    /// black-box `expected.yuv` holds.
    pub intra_edge_filter: bool,
}

impl Default for Svq3DecodeOptions {
    fn default() -> Self {
        Self {
            intra_edge_filter: true,
        }
    }
}

/// One decoded access unit.
#[derive(Debug, Clone)]
pub struct Svq3DecodedPicture {
    /// The reconstructed picture (macroblock-aligned canvas).
    pub picture: Svq3Picture,
    /// The slice type of the access unit.
    pub frame_type: Svq3FrameType,
    /// `picture_id` of its first slice.
    pub picture_id: u8,
}

/// The two chroma planes' decoded coefficient sets for one macroblock:
/// per plane, the four raw 2×2 DC levels plus the four placed 4×4 AC
/// level grids.
struct MbChromaCoeffs {
    cb_dc: [i32; 4],
    cb_ac: [[i32; 16]; 4],
    cr_dc: [i32; 4],
    cr_ac: [[i32; 16]; 4],
}

impl MbChromaCoeffs {
    fn zero() -> Self {
        Self {
            cb_dc: [0; 4],
            cb_ac: [[0; 16]; 4],
            cr_dc: [0; 4],
            cr_ac: [[0; 16]; 4],
        }
    }
}

/// Read the chroma section for chroma class `class` (spec/07 §6 items
/// 5–6 / §7 items 6–7 / spec/08 §6): the DC block of Cb then of Cr
/// when `class ≥ 1`, then the four AC blocks of Cb and the four of Cr
/// (scan position 1) when `class = 2`.
fn decode_chroma_section(br: &mut BitReader<'_>, class: u8) -> Result<MbChromaCoeffs> {
    let mut out = MbChromaCoeffs::zero();
    if class == 0 {
        return Ok(out);
    }
    decode_chroma_dc_2x2(br, &mut out.cb_dc)?;
    decode_chroma_dc_2x2(br, &mut out.cr_dc)?;
    if class >= 2 {
        for block in out.cb_ac.iter_mut().chain(out.cr_ac.iter_mut()) {
            decode_residual_4x4_normal(br, &NORMAL_ZIGZAG_4X4_SCAN, 1, block)?;
        }
    }
    Ok(out)
}

/// Read the coded luma quadrants of an intra 4×4 / flat-128 body
/// (spec/07 §6 item 4, §8): per coded 8×8 quadrant, four 4×4 blocks
/// in raster order, each a two-list alternate-scan block below
/// quantiser 24 and a single normal-zigzag list otherwise. The grids
/// are returned indexed by raster cell.
fn decode_intra_4x4_luma_blocks(
    br: &mut BitReader<'_>,
    cbp: CodedBlockPattern,
    qp: u32,
) -> Result<[[i32; 16]; 16]> {
    let mut coeff_blocks = [[0i32; 16]; 16];
    let use_alt = qp < ALT_SCAN_QUANTISER_THRESHOLD;
    for quadrant in 0..4usize {
        if !cbp.luma_quadrant_coded(quadrant) {
            continue;
        }
        for sub in 0..4usize {
            let cell = INTRA_4X4_BLOCK_RASTER[quadrant * 4 + sub] as usize;
            if use_alt {
                decode_residual_4x4_alt(br, &ALT_SCAN_4X4_SCAN, &mut coeff_blocks[cell])?;
            } else {
                decode_residual_4x4_normal(
                    br,
                    &NORMAL_ZIGZAG_4X4_SCAN,
                    0,
                    &mut coeff_blocks[cell],
                )?;
            }
        }
    }
    Ok(coeff_blocks)
}

/// The spec/07 §4 end-of-slice test, applied after every macroblock.
fn slice_ends(br: &BitReader<'_>, payload: &[u8]) -> bool {
    let pos = br.bits_consumed();
    let limit = payload.len() * 8;
    if pos + 7 < limit {
        return false;
    }
    if pos % 8 == 0 {
        return true;
    }
    let byte = payload.get(pos / 8).copied().unwrap_or(0);
    let remaining_mask = (1u16 << (8 - pos % 8)) as u8 - 1;
    byte & remaining_mask == 0
}

/// Component-wise median of three (spec/08 §4.3).
const fn median3(a: i32, b: i32, c: i32) -> i32 {
    let hi = if a > b { a } else { b };
    let lo = if a > b { b } else { a };
    if c > hi {
        hi
    } else if c < lo {
        lo
    } else {
        c
    }
}

/// Convert a predictor in sixths to the coded unit of `precision`
/// (spec/08 §4.2): full-pel `trunc((p + 3) / 6)` for `p ≥ 0`, else
/// `trunc((p − 2) / 6)`; half-pel the same applied to `2p`; third-pel
/// `(p + 1) >> 1`.
const fn convert_predictor(p: i32, precision: Svq3MvPrecision) -> i32 {
    match precision {
        Svq3MvPrecision::Fullpel => round_to_sixths(p),
        Svq3MvPrecision::Halfpel => round_to_sixths(2 * p),
        Svq3MvPrecision::Thirdpel => (p + 1) >> 1,
    }
}

const fn round_to_sixths(v: i32) -> i32 {
    if v >= 0 {
        (v + 3) / 6
    } else {
        (v - 2) / 6
    }
}

/// The stored-vector multiplier of a coded unit: ×6 full, ×3 half,
/// ×2 third (spec/08 §4.2).
const fn stored_scale(precision: Svq3MvPrecision) -> i32 {
    match precision {
        Svq3MvPrecision::Fullpel => 6,
        Svq3MvPrecision::Halfpel => 3,
        Svq3MvPrecision::Thirdpel => 2,
    }
}

/// Running state of one picture decode.
struct PictureState<'a> {
    seqh: &'a Svq3SequenceHeader,
    mb_cols: usize,
    mb_rows: usize,
    picture: Svq3Picture,
    reference: Option<&'a Svq3Picture>,
    /// Per-4×4-block map (row-major over the picture's `4·mb_cols ×
    /// 4·mb_rows` blocks): 0 = not decoded in the current slice,
    /// `mode + 1` for an intra 4×4 block, 1 for any other decoded
    /// block. Cleared at every slice start (spec/07 §10.2).
    block_map: Vec<u8>,
    /// Per-4×4-block motion vectors in sixths (spec/08 §4.2).
    mv: Vec<(i32, i32)>,
    frame_type: Svq3FrameType,
    /// The running macroblock quantiser (in place, spec/07 §9).
    qp: u32,
    mb_qp_delta_enable: bool,
}

impl<'a> PictureState<'a> {
    fn new(
        seqh: &'a Svq3SequenceHeader,
        reference: Option<&'a Svq3Picture>,
        frame_type: Svq3FrameType,
    ) -> Result<Self> {
        let (mb_cols, mb_rows) = mb_grid_dims(seqh);
        if mb_cols == 0 || mb_rows == 0 {
            return Err(Error::BadBitWidth(0));
        }
        let (mb_cols, mb_rows) = (mb_cols as usize, mb_rows as usize);
        let blocks = mb_cols * 4 * mb_rows * 4;
        Ok(Self {
            seqh,
            mb_cols,
            mb_rows,
            picture: Svq3Picture::new(mb_cols, mb_rows),
            reference,
            block_map: vec![0u8; blocks],
            mv: vec![(0, 0); blocks],
            frame_type,
            qp: 0,
            mb_qp_delta_enable: false,
        })
    }

    /// Block-map index of 4×4 block `(bx, by)` in block units.
    fn block_index(&self, bx: usize, by: usize) -> usize {
        by * self.mb_cols * 4 + bx
    }

    /// The `tables/08` contexts of the blocks above / left of the
    /// macroblock at `pos`.
    fn edge_contexts(&self, pos: Svq3MacroblockPosition) -> ([u8; 4], [u8; 4]) {
        let bx0 = pos.mb_x as usize * 4;
        let by0 = pos.mb_y as usize * 4;
        let mut top = [0u8; 4];
        let mut left = [0u8; 4];
        if pos.top_available {
            for (c, t) in top.iter_mut().enumerate() {
                *t = self.block_map[self.block_index(bx0 + c, by0 - 1)];
            }
        }
        if pos.left_available {
            for (r, l) in left.iter_mut().enumerate() {
                *l = self.block_map[self.block_index(bx0 - 1, by0 + r)];
            }
        }
        (top, left)
    }

    /// Mark the macroblock's sixteen blocks in the map (`value` per
    /// block, raster cells) and store its motion vector.
    fn mark_macroblock(&mut self, pos: Svq3MacroblockPosition, values: [u8; 16], mv: (i32, i32)) {
        let bx0 = pos.mb_x as usize * 4;
        let by0 = pos.mb_y as usize * 4;
        for (cell, &v) in values.iter().enumerate() {
            let i = self.block_index(bx0 + cell % 4, by0 + cell / 4);
            self.block_map[i] = v;
            self.mv[i] = mv;
        }
    }

    /// Apply a decoded quantiser delta in place (spec/07 §9), rejecting
    /// results outside the dequantisation-ladder domain.
    fn apply_quantiser_delta(&mut self, br: &mut BitReader<'_>) -> Result<()> {
        let delta = read_quantiser_delta(br)?;
        let next = self.qp as i64 + delta as i64;
        if next < 0 || next >= DEQUANT_COEFF_TABLE_LEN as i64 {
            return Err(Error::InvalidQuantiser(next as i32));
        }
        self.qp = next as u32;
        Ok(())
    }

    /// Reconstruct both chroma planes from `chroma` onto the intra DC
    /// prediction (or the flat 128 when `flat`) and blit them.
    fn reconstruct_intra_chroma(
        &mut self,
        pos: Svq3MacroblockPosition,
        chroma: &MbChromaCoeffs,
        flat: bool,
    ) {
        for (which, dc, ac) in [
            (ChromaSelect::Cb, chroma.cb_dc, &chroma.cb_ac),
            (ChromaSelect::Cr, chroma.cr_dc, &chroma.cr_ac),
        ] {
            let mut plane = ChromaPlane::new();
            self.picture.bind_chroma_neighbours(pos, which, &mut plane);
            let predicted = if flat {
                [128u8; PRED_CHROMA_SAMPLES]
            } else {
                predict_chroma_dc_8x8(
                    plane.above,
                    plane.leftcol,
                    plane.above_available,
                    plane.left_available,
                )
            };
            plane.samples = reconstruct_chroma_plane_with_prediction(&predicted, dc, ac, self.qp);
            self.picture.blit_chroma(pos, which, &plane);
        }
    }

    /// Decode + reconstruct one macroblock (spec/07 §5 dispatch).
    fn decode_macroblock(&mut self, br: &mut BitReader<'_>, mb_index: usize) -> Result<()> {
        let pos = macroblock_position(mb_index as u32, self.mb_cols as u32)?;
        match classify_mb_type(self.frame_type, read_universal_code(br)?)? {
            Svq3MbType::Intra(IntraMbKind::Intra4x4) => self.decode_intra_4x4_mb(br, pos, false),
            Svq3MbType::Intra(IntraMbKind::Flat128) => self.decode_intra_4x4_mb(br, pos, true),
            Svq3MbType::Intra(IntraMbKind::Intra16x16(params)) => {
                self.decode_intra_16x16_mb(br, pos, params)
            }
            Svq3MbType::PInter(mode) => self.decode_inter_mb(br, pos, mode),
            // B-slice bodies are not specified (spec/08 §7).
            Svq3MbType::BInter(_) => Err(Error::NotImplemented),
        }
    }

    /// The intra 4×4 body (spec/07 §6) — or, with `flat`, the flat-128
    /// body (§5: no prediction-mode elements, constant-128 prediction).
    fn decode_intra_4x4_mb(
        &mut self,
        br: &mut BitReader<'_>,
        pos: Svq3MacroblockPosition,
        flat: bool,
    ) -> Result<()> {
        let (top_ctx, left_ctx) = self.edge_contexts(pos);
        let grid = if flat {
            None
        } else {
            Some(decode_intra_4x4_modes_with_context(br, top_ctx, left_ctx)?)
        };

        let cbp = read_cbp_intra(br)?;
        if self.frame_type != Svq3FrameType::Intra && cbp.value() != 0 && self.mb_qp_delta_enable {
            self.apply_quantiser_delta(br)?;
        }
        let coeff_blocks = decode_intra_4x4_luma_blocks(br, cbp, self.qp)?;
        let chroma = decode_chroma_section(br, cbp.chroma)?;

        // Luma reconstruction, block by block in block order: predict
        // from the already-reconstructed neighbours (availability from
        // the per-slice map), add the residual, write back.
        let mut mb = LumaMacroblock::new();
        self.picture.bind_luma_neighbours(pos, &mut mb);
        mb.above_available = top_ctx[0] != 0;
        mb.left_available = left_ctx[0] != 0;
        for &cell in INTRA_4X4_BLOCK_RASTER.iter() {
            let cell = cell as usize;
            let by = (cell / 4) * PRED_4X4_DIM;
            let bx = (cell % 4) * PRED_4X4_DIM;
            let predicted = match &grid {
                Some(grid) => {
                    let mode = Svq3IntraMode::from_value(grid.modes()[cell])?;
                    predict_intra_4x4(mode, mb.neighbours_at(bx, by))?
                }
                None => [128u8; 16],
            };
            let residual = dequantize_transform_luma_block(self.qp, coeff_blocks[cell]);
            mb.write_block(bx, by, reconstruct_4x4(predicted, residual));
        }
        self.picture.blit_luma(pos, &mb);
        self.reconstruct_intra_chroma(pos, &chroma, flat);

        let mut values = [INTRA_4X4_CONTEXT_OTHER; 16];
        if let Some(grid) = grid {
            for (cell, v) in values.iter_mut().enumerate() {
                *v = grid.modes()[cell] + 1;
            }
        }
        self.mark_macroblock(pos, values, (0, 0));
        Ok(())
    }

    /// The intra 16×16 body (spec/07 §7).
    fn decode_intra_16x16_mb(
        &mut self,
        br: &mut BitReader<'_>,
        pos: Svq3MacroblockPosition,
        params: Intra16x16Params,
    ) -> Result<()> {
        // Item 1: the delta is always present, whatever the slice type
        // and the enable flag.
        self.apply_quantiser_delta(br)?;

        // Item 3: the separate luma DC block (spec/04 §4).
        let mut dc_block = [0i32; 16];
        decode_residual_4x4_normal(br, &NORMAL_ZIGZAG_4X4_SCAN, 0, &mut dc_block)?;
        let dc_terms = luma_dc_secondary_transform(self.qp, dc_block);

        // Item 4: sixteen luma AC lists, raster order, scan position 1.
        let mut ac_blocks = [[0i32; 16]; 16];
        if params.luma_ac {
            for block in ac_blocks.iter_mut() {
                decode_residual_4x4_normal(br, &NORMAL_ZIGZAG_4X4_SCAN, 1, block)?;
            }
        }

        // Items 6–7.
        let chroma = decode_chroma_section(br, params.cbp_chroma)?;

        let mut mb = LumaMacroblock::new();
        self.picture.bind_luma_neighbours(pos, &mut mb);
        let mode = Svq3Luma16x16Mode::from_pred_mode(
            params.pred_mode,
            pos.top_available,
            pos.left_available,
        )?;
        reconstruct_intra_16x16_luma_macroblock_with_dc(
            &mut mb, mode, &ac_blocks, &dc_terms, self.qp,
        );
        self.picture.blit_luma(pos, &mb);
        self.reconstruct_intra_chroma(pos, &chroma, false);
        self.mark_macroblock(pos, [INTRA_4X4_CONTEXT_OTHER; 16], (0, 0));
        Ok(())
    }

    /// Whether 4×4 block `(bx, by)` (block units, possibly outside the
    /// picture) is available for motion-vector prediction: inside the
    /// picture and decoded in the current slice (spec/08 §4.3).
    fn mv_block(&self, bx: i32, by: i32) -> Option<(i32, i32)> {
        let cols = (self.mb_cols * 4) as i32;
        let rows = (self.mb_rows * 4) as i32;
        if bx < 0 || by < 0 || bx >= cols || by >= rows {
            return None;
        }
        let i = self.block_index(bx as usize, by as usize);
        (self.block_map[i] != 0).then_some(self.mv[i])
    }

    /// The motion-vector predictor of spec/08 §4.3 for a `w × h`
    /// partition at luma position `(x, y)`: the median of the left
    /// (`A`), above (`B`) and above-right (`C`, replaced by above-left
    /// `D` when unavailable) blocks' vectors — that vector alone when
    /// exactly one is available, `(0, 0)` when none — then clamped so
    /// the referenced block lies inside the macroblock-aligned picture
    /// (all in sixths).
    fn predict_mv(&self, x: u32, y: u32, w: u32, h: u32) -> (i32, i32) {
        let bx = (x / 4) as i32;
        let by = (y / 4) as i32;
        let a = self.mv_block(bx - 1, by);
        let b = self.mv_block(bx, by - 1);
        let c = self
            .mv_block(bx + (w / 4) as i32, by - 1)
            .or_else(|| self.mv_block(bx - 1, by - 1));
        let available = [a, b, c].iter().filter(|v| v.is_some()).count();
        let (mut px, mut py) = match available {
            0 => (0, 0),
            1 => a.or(b).or(c).unwrap_or((0, 0)),
            _ => {
                let av = a.unwrap_or((0, 0));
                let bv = b.unwrap_or((0, 0));
                let cv = c.unwrap_or((0, 0));
                (median3(av.0, bv.0, cv.0), median3(av.1, bv.1, cv.1))
            }
        };
        // Clamp the referenced position to [0, 6·(W − w)] × [0, 6·(H − h)].
        let (wpic, hpic) = ((self.mb_cols * 16) as i32, (self.mb_rows * 16) as i32);
        let abs_x = (6 * x as i32 + px).clamp(0, 6 * (wpic - w as i32));
        let abs_y = (6 * y as i32 + py).clamp(0, 6 * (hpic - h as i32));
        px = abs_x - 6 * x as i32;
        py = abs_y - 6 * y as i32;
        (px, py)
    }

    /// Store `mv` for every 4×4 block of a `w × h` partition at `(x, y)`
    /// and mark the blocks decoded (context value 1).
    fn store_partition_mv(&mut self, x: u32, y: u32, w: u32, h: u32, mv: (i32, i32)) {
        for by in (y / 4)..((y + h) / 4) {
            for bx in (x / 4)..((x + w) / 4) {
                let i = self.block_index(bx as usize, by as usize);
                self.mv[i] = mv;
                self.block_map[i] = INTRA_4X4_CONTEXT_OTHER;
            }
        }
    }

    /// A P-slice inter macroblock (spec/08): the precision selector,
    /// one motion-vector-difference pair per partition (vertical
    /// first) predicted, clamped and converted per §4, the motion
    /// compensation of the whole macroblock (luma and both chroma
    /// planes), then the inter-table pattern, the conditional
    /// quantiser delta and the residual of §6. The skip type copies the
    /// co-located macroblock (§5).
    fn decode_inter_mb(
        &mut self,
        br: &mut BitReader<'_>,
        pos: Svq3MacroblockPosition,
        mode: PFrameInterMode,
    ) -> Result<()> {
        let reference = self.reference.ok_or(Error::MissingReference)?;
        let (mb_x, mb_y) = pos.luma_origin();

        if mode == PFrameInterMode::Skip {
            self.picture.copy_macroblock_from(reference, pos);
            self.mark_macroblock(pos, [INTRA_4X4_CONTEXT_OTHER; 16], (0, 0));
            return Ok(());
        }

        let precision =
            read_inter_mv_precision_p_frame(br, self.seqh.has_thirdpel, self.seqh.has_halfpel)?;
        let (w, h) = mode.partition_size();
        let mut luma = [0u8; 256];
        let mut cb = [0u8; 64];
        let mut cr = [0u8; 64];
        for (ox, oy) in mode.partition_offsets() {
            let mvd = read_mv_difference(br)?;
            let (x, y) = (mb_x + ox, mb_y + oy);
            let (pred_x, pred_y) = self.predict_mv(x, y, w, h);
            let mv = (
                convert_predictor(pred_x, precision) + mvd.dx,
                convert_predictor(pred_y, precision) + mvd.dy,
            );
            let stored = (
                mv.0 * stored_scale(precision),
                mv.1 * stored_scale(precision),
            );
            self.store_partition_mv(x, y, w, h, stored);

            let luma_ref = reference.luma_reference();
            let block = motion_compensate_block(
                &luma_ref, x as i32, y as i32, w as usize, h as usize, stored.0, stored.1,
            );
            for r in 0..h as usize {
                for c in 0..w as usize {
                    luma[(oy as usize + r) * 16 + ox as usize + c] = block[r * w as usize + c];
                }
            }
            for (plane, which) in [(&mut cb, ChromaSelect::Cb), (&mut cr, ChromaSelect::Cr)] {
                let chroma_ref = reference.chroma_reference(which);
                let block = motion_compensate_chroma_block(
                    &chroma_ref,
                    x as i32,
                    y as i32,
                    w as usize,
                    h as usize,
                    stored.0,
                    stored.1,
                );
                let (cw, chh) = (w as usize / 2, h as usize / 2);
                for r in 0..chh {
                    for c in 0..cw {
                        plane[(oy as usize / 2 + r) * 8 + ox as usize / 2 + c] = block[r * cw + c];
                    }
                }
            }
        }

        // §6: pattern (inter table), conditional delta, residual.
        let cbp = read_cbp_inter(br)?;
        if cbp.value() != 0 && self.mb_qp_delta_enable {
            self.apply_quantiser_delta(br)?;
        }
        let mut coeff_blocks = [[0i32; 16]; 16];
        for quadrant in 0..4usize {
            if !cbp.luma_quadrant_coded(quadrant) {
                continue;
            }
            for sub in 0..4usize {
                let cell = INTRA_4X4_BLOCK_RASTER[quadrant * 4 + sub] as usize;
                decode_residual_4x4_normal(
                    br,
                    &NORMAL_ZIGZAG_4X4_SCAN,
                    0,
                    &mut coeff_blocks[cell],
                )?;
            }
        }
        let chroma = decode_chroma_section(br, cbp.chroma)?;

        add_luma_residual_blocks(&mut luma, &coeff_blocks, self.qp);
        let mut mb = LumaMacroblock::new();
        mb.samples = luma;
        self.picture.blit_luma(pos, &mb);
        for (which, predicted, dc, ac) in [
            (ChromaSelect::Cb, &cb, chroma.cb_dc, &chroma.cb_ac),
            (ChromaSelect::Cr, &cr, chroma.cr_dc, &chroma.cr_ac),
        ] {
            let mut plane = ChromaPlane::new();
            plane.samples = reconstruct_chroma_plane_with_prediction(predicted, dc, ac, self.qp);
            self.picture.blit_chroma(pos, which, &plane);
        }
        Ok(())
    }

    /// Decode the macroblocks of one slice (spec/07 §4), returning the
    /// cursor after its last macroblock.
    fn decode_slice(
        &mut self,
        header: &Svq3SliceHeader,
        payload: &[u8],
        cursor: usize,
    ) -> Result<usize> {
        if header.mode {
            // The extended macroblock-layer mode prefixes every
            // macroblock by a u(3) whose semantics are not specified
            // (spec/07 §3.3).
            return Err(Error::NotImplemented);
        }
        let total = self.mb_cols * self.mb_rows;
        let start = header.first_mb.map(|m| m as usize).unwrap_or(0);
        if start < cursor || start >= total {
            return Err(Error::InvalidFrameCode(start as u32));
        }
        self.block_map.fill(0);
        self.qp = u32::from(header.slice_qp);
        self.mb_qp_delta_enable = header.mb_qp_delta_enable;

        let mut br = BitReader::new(payload);
        for _ in 0..header.header_end_bit {
            br.read_bit()?;
        }
        let mut mb = start;
        loop {
            self.decode_macroblock(&mut br, mb)?;
            mb += 1;
            if mb == total || slice_ends(&br, payload) {
                break;
            }
        }
        Ok(mb)
    }
}

/// A stateful SVQ3 picture decoder: holds the stream header, the
/// reference picture for P access units, and the decode options.
#[derive(Debug, Clone)]
pub struct Svq3PictureDecoder {
    seqh: Svq3SequenceHeader,
    reference: Option<Svq3Picture>,
    options: Svq3DecodeOptions,
}

impl Svq3PictureDecoder {
    /// Create a decoder for the stream described by `seqh`.
    ///
    /// Returns [`Error::BadBitWidth`] for a zero-sized picture.
    pub fn new(seqh: Svq3SequenceHeader) -> Result<Self> {
        let (mb_cols, mb_rows) = mb_grid_dims(&seqh);
        if mb_cols == 0 || mb_rows == 0 {
            return Err(Error::BadBitWidth(0));
        }
        Ok(Self {
            seqh,
            reference: None,
            options: Svq3DecodeOptions::default(),
        })
    }

    /// Override the decode options.
    #[must_use]
    pub fn with_options(mut self, options: Svq3DecodeOptions) -> Self {
        self.options = options;
        self
    }

    /// The decode options.
    #[must_use]
    pub fn options(&self) -> Svq3DecodeOptions {
        self.options
    }

    /// Change the decode options.
    pub fn set_options(&mut self, options: Svq3DecodeOptions) {
        self.options = options;
    }

    /// The stream header.
    #[must_use]
    pub fn sequence_header(&self) -> &Svq3SequenceHeader {
        &self.seqh
    }

    /// The current reference picture, if any picture has been decoded.
    #[must_use]
    pub fn reference(&self) -> Option<&Svq3Picture> {
        self.reference.as_ref()
    }

    /// Drop the reference picture (a seek / stream restart).
    pub fn reset(&mut self) {
        self.reference = None;
    }

    /// Decode one access unit (spec/07 §2: a sequence of packets ending
    /// at the `0xff` marker or the end of `au`) into a picture, which
    /// also becomes the reference for the next access unit.
    ///
    /// Returns [`Error::NotImplemented`] for B slices and for the
    /// extended macroblock-layer mode, [`Error::MissingReference`] for a
    /// P access unit with no preceding picture, [`Error::Truncated`]
    /// when an I access unit ends before its macroblock grid is
    /// complete, and the per-macroblock structural errors of the
    /// composed layers.
    pub fn decode_access_unit(&mut self, au: &[u8]) -> Result<Svq3DecodedPicture> {
        let total = num_macroblocks(&self.seqh) as usize;
        let mut offset = 0usize;
        let mut cursor = 0usize;
        let mut state: Option<PictureState<'_>> = None;
        let mut picture_id = 0u8;

        while offset < au.len() {
            let packet_byte = au[offset];
            match classify_packet_byte(packet_byte)? {
                Svq3PacketKind::End => break,
                Svq3PacketKind::Zero { length_width } => {
                    // spec/07 §2: type 0 is followed by a u(16) that must
                    // be zero; nothing else about it is specified.
                    let end = offset + 1 + length_width as usize + 2;
                    if au.len() < end {
                        return Err(Error::Truncated);
                    }
                    if au[end - 2] != 0 || au[end - 1] != 0 {
                        return Err(Error::InvalidFrameCode(u16::from_be_bytes([
                            au[end - 2],
                            au[end - 1],
                        ]) as u32));
                    }
                    offset = end;
                    continue;
                }
                Svq3PacketKind::Slice { .. } => {}
            }
            let slice = parse_wire_slice(
                &au[offset..],
                total as u32,
                self.seqh.protected,
                self.seqh.extended_mode,
            )?;
            offset += slice.consumed;

            let st = match state.as_mut() {
                Some(st) => {
                    if st.frame_type != slice.header.frame_type {
                        return Err(Error::InvalidFrameCode(slice.header.frame_type as u32));
                    }
                    st
                }
                None => {
                    let frame_type = slice.header.frame_type;
                    picture_id = slice.header.picture_id;
                    let reference = match frame_type {
                        Svq3FrameType::Intra => None,
                        Svq3FrameType::Predicted => {
                            Some(self.reference.as_ref().ok_or(Error::MissingReference)?)
                        }
                        Svq3FrameType::Bidirectional => return Err(Error::NotImplemented),
                    };
                    state.insert(PictureState::new(&self.seqh, reference, frame_type)?)
                }
            };
            cursor = st.decode_slice(&slice.header, &slice.payload, cursor)?;
            if cursor >= total {
                break;
            }
        }

        let Some(mut st) = state else {
            return Err(Error::Truncated);
        };
        if cursor < total {
            match st.frame_type {
                Svq3FrameType::Intra => return Err(Error::Truncated),
                // spec/08 §7: the macroblocks following a P slice's last
                // macroblock are copied from the reference at zero
                // motion (range not verified).
                Svq3FrameType::Predicted => {
                    if let Some(reference) = st.reference {
                        for mb in cursor..total {
                            let pos = macroblock_position(mb as u32, st.mb_cols as u32)?;
                            st.picture.copy_macroblock_from(reference, pos);
                        }
                    }
                }
                Svq3FrameType::Bidirectional => return Err(Error::NotImplemented),
            }
        }

        let frame_type = st.frame_type;
        let mut picture = st.picture;
        if frame_type == Svq3FrameType::Intra && self.options.intra_edge_filter {
            // spec/09 §1–§2: after the last macroblock of an I picture,
            // with the luma quantiser then in force.
            crate::svq3_filter::filter_intra_picture(&mut picture, st.qp);
        }
        self.reference = Some(picture.clone());
        Ok(Svq3DecodedPicture {
            picture,
            frame_type,
            picture_id,
        })
    }
}

/// Decode one SVQ3 **intra** access unit into a reconstructed picture
/// with the edge filter disabled — the unfiltered reconstruction of
/// spec/01, 04 and 06–08 (what the fixtures' black-box reference
/// decode holds). Any other slice type is [`Error::NotImplemented`].
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn decode_intra_access_unit(seqh: &Svq3SequenceHeader, au: &[u8]) -> Result<Svq3Picture> {
    let mut dec = Svq3PictureDecoder::new(seqh.clone())?.with_options(Svq3DecodeOptions {
        intra_edge_filter: false,
    });
    let decoded = dec.decode_access_unit(au)?;
    if decoded.frame_type != Svq3FrameType::Intra {
        return Err(Error::NotImplemented);
    }
    Ok(decoded.picture)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svq3::{parse_extradata, SVQ3_FRAME_END};
    use crate::svq3_testutil::Packer;

    /// A 32×32 (2×2 macroblock) SEQH via the explicit-dimension escape.
    fn seqh_32x32() -> Svq3SequenceHeader {
        let mut p = Packer::new();
        p.push(3, 7); // frame_size_code = 7 → explicit dims
        p.push(12, 32); // width
        p.push(12, 32); // height
        p.push(1, 0); // halfpel
        p.push(1, 0); // thirdpel
        p.push(1, 0); // postfilter hint
        p.push(1, 0); // extended mode
        p.push(2, 0b11); // reserved
        p.push(1, 1); // no B frames
        p.push(1, 0); // reserved
        p.push(1, 0); // no optional data
        p.push(1, 0); // not protected
        let payload = p.into_bytes();
        let mut extradata = Vec::new();
        extradata.extend_from_slice(b"SEQH");
        extradata.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        extradata.extend_from_slice(&payload);
        parse_extradata(&extradata).unwrap()
    }

    /// Start a packet-type-1 slice header: the given slice type,
    /// encrypted 0, picture_id 0, quantiser, delta enable, flag 0,
    /// mode 0, reserved, no extension bytes.
    fn slice_header(p: &mut Packer, slice_type: u32, qp: u32, delta_enable: bool) {
        p.ue(slice_type);
        p.push(1, 0); // encrypted
        p.push(8, 0); // picture_id
        p.push(5, qp);
        p.push(1, u32::from(delta_enable)); // mb_qp_delta_enable
        p.push(1, 0); // flag
        p.push(1, 0); // mode
        p.push(2, 0); // reserved
        p.push(1, 0); // extension bytes: none
    }

    fn intra_slice_header(p: &mut Packer, qp: u32, delta_enable: bool) {
        slice_header(p, 2, qp, delta_enable);
    }

    /// Wrap a packed slice payload in the type-1 wire envelope (packet
    /// byte, 2-byte length, relocated body) + the end marker.
    fn wire_v1(payload: Vec<u8>) -> Vec<u8> {
        let l = 2u8;
        let mut au = vec![(l << 5) | 1];
        au.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        // The payload's first byte travels at the end of the packet.
        au.extend_from_slice(&payload[1..]);
        au.push(payload[0]);
        au.push(SVQ3_FRAME_END);
        au
    }

    /// One all-empty intra-4×4 macroblock: type 0, eight pair codes 0,
    /// pattern code 3 (pattern 0) — the 14-bit unit of spec/07 §11.
    fn push_empty_i4_mb(p: &mut Packer) {
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        p.ue(3);
    }

    #[test]
    fn empty_i4_macroblock_is_14_bits() {
        let mut p = Packer::new();
        push_empty_i4_mb(&mut p);
        assert_eq!(p.len(), 14);
    }

    #[test]
    fn all_empty_intra4x4_frame_decodes_flat() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        for _ in 0..4 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        assert!(pic.luma().iter().all(|&s| s == 128));
        assert!(pic.cb().iter().all(|&s| s == 128));
        assert!(pic.cr().iter().all(|&s| s == 128));
    }

    #[test]
    fn worked_example_macroblock_0_of_fixture_320x240() {
        // spec/07 §11: type 0, eight pair codes 1, pattern code 29 →
        // pattern 1, then quadrant 0's four alternate-scan two-list
        // blocks: block 0 list 1 = escape code 608 (level −46 at
        // position 0) then end, list 2 end; blocks 1–3 six ends. 45
        // bits. Level −46 at quantiser 13 reconstructs to −129 on a 128
        // predictor — black.
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        let start = p.len();
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        p.ue(29);
        p.ue(608);
        p.ue(0);
        p.ue(0);
        for _ in 0..6 {
            p.ue(0);
        }
        assert_eq!(p.len() - start, 45);
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        // Block (0, 0) is black; the rest of quadrant 0 predicts DC from
        // black/128 neighbours.
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(pic.luma_sample(x, y), 0, "({x},{y})");
            }
        }
        // Block (4, 0): only its left neighbour (black) is available →
        // DC 0.
        assert_eq!(pic.luma_sample(4, 0), 0);
        // Block (0, 4): only its top neighbour (black) → 0.
        assert_eq!(pic.luma_sample(0, 4), 0);
        // Block (4, 4): both black → 0.
        assert_eq!(pic.luma_sample(4, 4), 0);
        // Quadrant 1's block (8, 0): left neighbour is block (4, 0) = 0,
        // no top → 0. Chroma untouched.
        assert_eq!(pic.luma_sample(8, 0), 0);
        assert!(pic.cb().iter().all(|&s| s == 128));
    }

    #[test]
    fn intra4x4_dc_level_one_at_qp13_adds_three() {
        // spec/06 §4 worked instance: a lone level +1 at the DC
        // position of an intra 4×4 block at quantiser 13 is 17435,
        // transforms to (17435·169 + 0x80000) >> 20 = 3, and adds 3 to
        // every sample of the block — no separate intra DC scale.
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        p.ue(29); // pattern 1: top-left quadrant only
        p.ue(1); // block 0, list 1: level +1 run 0
        p.ue(0); // list 1 end
        p.ue(0); // list 2 end
        for _ in 0..6 {
            p.ue(0);
        }
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(pic.luma_sample(x, y), 131, "({x},{y})");
            }
        }
    }

    #[test]
    fn dc_only_16x16_frame_reconstructs_uniform_shift() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        // MB0: I code 1 → record 0 (pred DC, no chroma, no luma AC);
        // the always-present delta (0); DC block = one coefficient —
        // normal-book code 15 = level +3 run 0 per tables/05 — then
        // end.
        p.ue(1);
        p.se(0);
        p.ue(15);
        p.ue(0);
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();

        let mut dc_block = [0i32; 16];
        dc_block[0] = 3;
        let v = luma_dc_secondary_transform(13, dc_block);
        let expected =
            crate::svq3_pred::reconstruct_sample(128, crate::svq3_dequant::finalise_dc(169 * v[0]));
        assert_ne!(expected, 128, "test premise: the DC shift must be visible");
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(pic.luma_sample(x, y), expected, "({x},{y})");
            }
        }
        // MB1 (top-right) is an empty intra-4×4 whose left neighbour is
        // the shifted MB0 — its DC-mode blocks propagate from the seam.
        assert_eq!(pic.luma_sample(16, 0), expected);
        assert!(pic.cb().iter().all(|&s| s == 128));
    }

    #[test]
    fn chroma_section_orders_dc_pair_before_ac_blocks() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        // MB0: I code 9 → record 8 = (pred 0 DC, chroma class 2, no
        // luma AC). Delta 0, empty luma DC block, then Cb DC (+3),
        // Cr DC (+3), then eight empty AC lists.
        p.ue(9);
        p.se(0);
        p.ue(0); // luma DC end
        p.ue(7); // Cb DC level +3 (chroma book code 7)
        p.ue(0);
        p.ue(7); // Cr DC level +3
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();

        let b = crate::svq3_dequant::dequantize_chroma_dc_levels(13, [3, 0, 0, 0]);
        let expected =
            crate::svq3_pred::reconstruct_sample(128, crate::svq3_dequant::finalise_dc(169 * b[0]));
        assert_ne!(expected, 128);
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(pic.cb_sample(x, y), expected, "cb ({x},{y})");
                assert_eq!(pic.cr_sample(x, y), expected, "cr ({x},{y})");
            }
        }
        assert_eq!(pic.luma_sample(0, 0), 128);
    }

    #[test]
    fn i_slice_intra4x4_never_carries_a_delta() {
        // Delta enable set, non-zero pattern, I slice: no delta element
        // (spec/07 §9). Block 0 carries +1 at the DC; the next
        // macroblock must parse cleanly.
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, true);
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        p.ue(29);
        p.ue(1);
        p.ue(0);
        p.ue(0);
        for _ in 0..6 {
            p.ue(0);
        }
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        assert_eq!(pic.luma_sample(0, 0), 131);
        // Inserting a delta element where the I slice has none must
        // desynchronise the walk (the AU no longer parses to its end).
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, true);
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        p.ue(29);
        p.se(1);
        p.ue(1);
        p.ue(0);
        p.ue(0);
        for _ in 0..6 {
            p.ue(0);
        }
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let other = decode_intra_access_unit(&seqh, &au);
        assert!(other.is_err() || other.unwrap().luma() != pic.luma());
    }

    #[test]
    fn intra16x16_delta_updates_running_quantiser_and_bounds() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 10, false); // enable flag irrelevant
        p.ue(1);
        p.se(4); // quantiser delta +4 → 14
        p.ue(15); // DC level +3
        p.ue(0);
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        let mut dc_block = [0i32; 16];
        dc_block[0] = 3;
        let v = luma_dc_secondary_transform(14, dc_block);
        let expected =
            crate::svq3_pred::reconstruct_sample(128, crate::svq3_dequant::finalise_dc(169 * v[0]));
        assert_eq!(pic.luma_sample(0, 0), expected);

        let mut p = Packer::new();
        intra_slice_header(&mut p, 30, false);
        p.ue(1);
        p.se(4); // 30 + 4 = 34 → out of range
        let au = wire_v1(p.into_bytes());
        assert_eq!(
            decode_intra_access_unit(&seqh, &au).unwrap_err(),
            Error::InvalidQuantiser(34)
        );
    }

    #[test]
    fn i_slice_type_code_25_is_rejected() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        p.ue(25);
        p.ue(0);
        let au = wire_v1(p.into_bytes());
        assert_eq!(
            decode_intra_access_unit(&seqh, &au).unwrap_err(),
            Error::InvalidFrameCode(25)
        );
    }

    #[test]
    fn intra4x4_mode_context_crosses_macroblocks() {
        // Four macroblocks; MB1 codes rank 1 for block 0 with the left
        // neighbour (MB0's DC blocks, context 1) and no top (context
        // 0): row (0, 1) rank 1 → horizontal, which reads MB0's right
        // column. MB0's block (12, 0) region is lifted by a DC
        // coefficient so the copy is visible.
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        // MB0: pattern 8 → bottom-right quadrant... use pattern code 5
        // → intra pattern 27 = luma 11 (quadrants 0, 1, 3) chroma 1.
        // Simpler: pattern code 2 → 15 (all quadrants, no chroma) with
        // block 5 (cell 3 = (12, 0)) carrying +1.
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        p.ue(2);
        for block in 0..16 {
            if block == 5 {
                p.ue(1);
                p.ue(0);
                p.ue(0);
            } else {
                p.ue(0);
                p.ue(0);
            }
        }
        // MB1: rank pair code 1 → ranks (1, 0) for blocks 0 and 1.
        p.ue(0);
        p.ue(1);
        for _ in 0..7 {
            p.ue(0);
        }
        p.ue(3);
        push_empty_i4_mb(&mut p);
        push_empty_i4_mb(&mut p);
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        // Cell 3 of MB0 = block (12, 0): +3.
        assert_eq!(pic.luma_sample(12, 0), 131);
        assert_eq!(pic.luma_sample(15, 3), 131);
        // MB1 block 0 is horizontal from column 15: rows 0..4 copy 131.
        for y in 0..4 {
            assert_eq!(pic.luma_sample(16, y), 131, "row {y}");
            assert_eq!(pic.luma_sample(19, y), 131, "row {y}");
        }
    }

    #[test]
    fn slice_ends_rule() {
        // spec/07 §11: the 300th macroblock ends at bit 4254 of a
        // 532-byte payload (limit 4256); the two remaining bits of the
        // byte are zero → the slice ends.
        let mut payload = vec![0xffu8; 532];
        payload[531] = 0b1111_1100;
        let mut br = BitReader::new(&payload);
        for _ in 0..4254 {
            br.read_bit().unwrap();
        }
        assert!(slice_ends(&br, &payload));
        // With a set bit remaining the slice continues.
        payload[531] = 0b1111_1110;
        let mut br = BitReader::new(&payload);
        for _ in 0..4254 {
            br.read_bit().unwrap();
        }
        assert!(!slice_ends(&br, &payload));
        // More than 7 bits left: never ends.
        let mut br = BitReader::new(&payload);
        for _ in 0..4248 {
            br.read_bit().unwrap();
        }
        assert!(!slice_ends(&br, &payload));
        // Exactly at the end (byte-aligned): ends.
        let mut br = BitReader::new(&payload);
        for _ in 0..4256 {
            br.read_bit().unwrap();
        }
        assert!(slice_ends(&br, &payload));
    }

    #[test]
    fn truncated_access_unit_errors() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        push_empty_i4_mb(&mut p); // only 1 of 4 macroblocks
        let au = wire_v1(p.into_bytes());
        assert_eq!(
            decode_intra_access_unit(&seqh, &au).unwrap_err(),
            Error::Truncated
        );
        assert_eq!(
            decode_intra_access_unit(&seqh, &[]).unwrap_err(),
            Error::Truncated
        );
    }

    #[test]
    fn p_slice_without_reference_is_rejected() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        slice_header(&mut p, 0, 13, false);
        p.ue(0);
        let au = wire_v1(p.into_bytes());
        let mut dec = Svq3PictureDecoder::new(seqh).unwrap();
        assert_eq!(
            dec.decode_access_unit(&au).unwrap_err(),
            Error::MissingReference
        );
    }

    #[test]
    fn b_slice_is_not_implemented() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        slice_header(&mut p, 1, 13, false);
        p.ue(0);
        let au = wire_v1(p.into_bytes());
        let mut dec = Svq3PictureDecoder::new(seqh).unwrap();
        assert_eq!(
            dec.decode_access_unit(&au).unwrap_err(),
            Error::NotImplemented
        );
    }

    #[test]
    fn bitflip_and_truncation_error_cleanly() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        p.ue(1);
        p.se(0);
        p.ue(15);
        p.ue(0);
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        for len in 0..au.len() {
            let _ = decode_intra_access_unit(&seqh, &au[..len]);
        }
        for byte in 0..au.len() {
            for bit in 0..8 {
                let mut m = au.clone();
                m[byte] ^= 1 << bit;
                let _ = decode_intra_access_unit(&seqh, &m);
            }
        }
    }

    #[test]
    fn intra_16x16_luma_ac_matches_component_composition() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        // MB0: I code 13 → record 12 = (pred 0 DC, chroma 0, luma_ac).
        // Delta 0; DC block: level +3 (code 15), end; sixteen AC lists
        // from scan position 1: block 0 carries level +1 run 0 (raster
        // 1), the other fifteen are empty.
        p.ue(13);
        p.se(0);
        p.ue(15);
        p.ue(0);
        p.ue(1);
        p.ue(0);
        for _ in 0..15 {
            p.ue(0);
        }
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();

        let mut dc_block = [0i32; 16];
        dc_block[0] = 3;
        let v = luma_dc_secondary_transform(13, dc_block);
        let mut ac_blocks = [[0i32; 16]; 16];
        ac_blocks[0][NORMAL_ZIGZAG_4X4_SCAN[1]] = 1;
        let mut mb = LumaMacroblock::new();
        reconstruct_intra_16x16_luma_macroblock_with_dc(
            &mut mb,
            Svq3Luma16x16Mode::Dc,
            &ac_blocks,
            &v,
            13,
        );
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(pic.luma_sample(x, y), mb.samples[y * 16 + x], "({x},{y})");
            }
        }
        assert_ne!(pic.luma_sample(1, 0), pic.luma_sample(0, 0));
    }

    #[test]
    fn intra16x16_directional_mode_at_picture_edge_is_rejected() {
        // Record 1 = vertical at the top-left macroblock: no top row.
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        p.ue(2);
        p.se(0);
        p.ue(0);
        let au = wire_v1(p.into_bytes());
        assert_eq!(
            decode_intra_access_unit(&seqh, &au).unwrap_err(),
            Error::MissingIntraNeighbour(1)
        );
    }

    /// Start a packet-type-2 slice header with `first_mb` (6-bit field
    /// for a 4-macroblock picture).
    fn intra_slice_header_v2(p: &mut Packer, first_mb: u32, qp: u32) {
        p.ue(2);
        p.push(6, first_mb);
        p.push(8, 0);
        p.push(5, qp);
        p.push(1, 0);
        p.push(1, 0);
        p.push(1, 0); // mode
        p.push(2, 0);
        p.push(1, 0);
    }

    /// Wrap a packed slice payload in the type-2 wire envelope (no end
    /// marker — the caller concatenates).
    fn wire_v2_slice(payload: Vec<u8>) -> Vec<u8> {
        let l = 2u8;
        let mut out = vec![(l << 5) | 2];
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&payload[1..]);
        out.push(payload[0]);
        out
    }

    #[test]
    fn v2_two_slice_access_unit_decodes() {
        // Slice 0: macroblocks 0..2 (ends by the §4 rule at its
        // zero-padded byte boundary); slice 1: macroblocks 2..4 at
        // first_mb 2.
        let seqh = seqh_32x32();
        let mut p0 = Packer::new();
        intra_slice_header_v2(&mut p0, 0, 13);
        push_empty_i4_mb(&mut p0);
        push_empty_i4_mb(&mut p0);
        let mut p1 = Packer::new();
        intra_slice_header_v2(&mut p1, 2, 13);
        push_empty_i4_mb(&mut p1);
        push_empty_i4_mb(&mut p1);
        let mut au = wire_v2_slice(p0.into_bytes());
        au.extend_from_slice(&wire_v2_slice(p1.into_bytes()));
        au.push(SVQ3_FRAME_END);
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        assert!(pic.luma().iter().all(|&s| s == 128));

        // A second slice that goes backwards is rejected.
        let mut p0 = Packer::new();
        intra_slice_header_v2(&mut p0, 0, 13);
        push_empty_i4_mb(&mut p0);
        push_empty_i4_mb(&mut p0);
        let mut p1 = Packer::new();
        intra_slice_header_v2(&mut p1, 1, 13);
        push_empty_i4_mb(&mut p1);
        let mut au = wire_v2_slice(p0.into_bytes());
        au.extend_from_slice(&wire_v2_slice(p1.into_bytes()));
        au.push(SVQ3_FRAME_END);
        assert_eq!(
            decode_intra_access_unit(&seqh, &au).unwrap_err(),
            Error::InvalidFrameCode(1)
        );
    }

    #[test]
    fn second_slice_clears_the_block_map() {
        // MB2 (second row, first column) in a new slice: its top
        // neighbour MB0 is decoded but in the previous slice, so a
        // vertical rank there is illegal (context 0).
        let seqh = seqh_32x32();
        let mut p0 = Packer::new();
        intra_slice_header_v2(&mut p0, 0, 13);
        push_empty_i4_mb(&mut p0);
        push_empty_i4_mb(&mut p0);
        let mut p1 = Packer::new();
        intra_slice_header_v2(&mut p1, 2, 13);
        p1.ue(0);
        p1.ue(1); // ranks (1, 0): rank 1 with contexts (0, 0) → illegal
        for _ in 0..7 {
            p1.ue(0);
        }
        p1.ue(3);
        push_empty_i4_mb(&mut p1);
        let mut au = wire_v2_slice(p0.into_bytes());
        au.extend_from_slice(&wire_v2_slice(p1.into_bytes()));
        au.push(SVQ3_FRAME_END);
        assert_eq!(
            decode_intra_access_unit(&seqh, &au).unwrap_err(),
            Error::InvalidIntraPrediction(0, 0, 1)
        );
        // In one slice the same rank is legal (top context 1 → vertical).
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        push_empty_i4_mb(&mut p);
        push_empty_i4_mb(&mut p);
        p.ue(0);
        p.ue(1);
        for _ in 0..7 {
            p.ue(0);
        }
        p.ue(3);
        push_empty_i4_mb(&mut p);
        let au = wire_v1(p.into_bytes());
        decode_intra_access_unit(&seqh, &au).unwrap();
    }

    #[test]
    fn intra4x4_alt_scan_two_lists_are_uncapped() {
        // qp 13 < 24 → two-list blocks. Block 0's first list carries a
        // run that crosses position 7 (run 9 lands at alternate-scan
        // position 9 = raster 8), which a per-half cap would reject.
        // Alternate book: code number for (level +1, run 9)? Use the
        // escape construction: run 9 is beyond the 8-run alphabet of
        // the alternate book's tabulated part — use two short runs
        // instead: (+1, run 4) then (+1, run 4): positions 4 and 9.
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        p.ue(29); // pattern 1
                  // tables/05 alternate book: code 11 = (+1, run 4)? Resolve via
                  // the book at runtime instead of hard-coding.
        let code_plus1_run4 = (1u32..31)
            .find(|&c| {
                crate::svq3_coeff::resolve_level_run(
                    crate::svq3_coeff::ResidualBook::AlternateScan,
                    c,
                ) == Some((1, 4))
            })
            .expect("alternate book carries (+1, run 4)");
        p.ue(code_plus1_run4);
        p.ue(code_plus1_run4);
        p.ue(0); // list 1 end
        p.ue(0); // list 2 end
        for _ in 0..6 {
            p.ue(0);
        }
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        // Two AC coefficients landed (raster positions of alternate-scan
        // slots 4 and 9); the block is no longer flat.
        let block: Vec<u8> = (0..4)
            .flat_map(|y| (0..4).map(move |x| (x, y)))
            .map(|(x, y)| pic.luma_sample(x, y))
            .collect();
        assert!(block.iter().any(|&s| s != block[0]));
    }

    #[test]
    fn predictor_conversion_matches_spec08() {
        // spec/08 §4.2: full-pel trunc((p+3)/6) / trunc((p−2)/6).
        assert_eq!(convert_predictor(0, Svq3MvPrecision::Fullpel), 0);
        assert_eq!(convert_predictor(3, Svq3MvPrecision::Fullpel), 1);
        assert_eq!(convert_predictor(2, Svq3MvPrecision::Fullpel), 0);
        assert_eq!(convert_predictor(-3, Svq3MvPrecision::Fullpel), 0);
        assert_eq!(convert_predictor(-4, Svq3MvPrecision::Fullpel), -1);
        assert_eq!(convert_predictor(9, Svq3MvPrecision::Fullpel), 2);
        // Half-pel: the same on 2p.
        assert_eq!(convert_predictor(3, Svq3MvPrecision::Halfpel), 1);
        assert_eq!(convert_predictor(1, Svq3MvPrecision::Halfpel), 0);
        assert_eq!(convert_predictor(2, Svq3MvPrecision::Halfpel), 1);
        assert_eq!(convert_predictor(-2, Svq3MvPrecision::Halfpel), -1);
        assert_eq!(convert_predictor(-1, Svq3MvPrecision::Halfpel), 0);
        // Third-pel: (p + 1) >> 1.
        assert_eq!(convert_predictor(2, Svq3MvPrecision::Thirdpel), 1);
        assert_eq!(convert_predictor(1, Svq3MvPrecision::Thirdpel), 1);
        assert_eq!(convert_predictor(-1, Svq3MvPrecision::Thirdpel), 0);
        assert_eq!(convert_predictor(-2, Svq3MvPrecision::Thirdpel), -1); // arithmetic shift floors
        assert_eq!(convert_predictor(-3, Svq3MvPrecision::Thirdpel), -1);
        assert_eq!(stored_scale(Svq3MvPrecision::Fullpel), 6);
        assert_eq!(stored_scale(Svq3MvPrecision::Halfpel), 3);
        assert_eq!(stored_scale(Svq3MvPrecision::Thirdpel), 2);
        assert_eq!(median3(1, 5, 3), 3);
        assert_eq!(median3(5, 1, 3), 3);
        assert_eq!(median3(-4, -4, 9), -4);
        assert_eq!(median3(2, 2, 2), 2);
    }

    #[test]
    fn mv_predictor_availability_median_and_clamp() {
        let seqh = seqh_32x32();
        let mut st = PictureState::new(&seqh, None, Svq3FrameType::Predicted).unwrap();
        // Nothing decoded: (0, 0).
        assert_eq!(st.predict_mv(16, 16, 16, 16), (0, 0));
        // Only A (left macroblock) decoded: its vector alone (inside
        // the clamp window: 96 − 6 and 96 − 12 sixths).
        st.store_partition_mv(0, 16, 16, 16, (-6, -12));
        assert_eq!(st.predict_mv(16, 16, 16, 16), (-6, -12));
        // A and B: C = above-right (32, 12) is outside the picture and
        // D = above-left (12, 12) is undecoded → median(A, B, (0, 0)).
        st.store_partition_mv(16, 0, 16, 16, (18, 6));
        assert_eq!(st.predict_mv(16, 16, 16, 16), (0, 0));
        // D available → median(A, B, D).
        st.store_partition_mv(0, 0, 16, 16, (-30, -30));
        assert_eq!(st.predict_mv(16, 16, 16, 16), (-6, -12));
        // Clamp: a predictor pointing left of the picture for the
        // macroblock at x = 0 is pulled to 0; at the right edge the
        // referenced block must end inside the 32-wide picture.
        let mut st = PictureState::new(&seqh, None, Svq3FrameType::Predicted).unwrap();
        st.store_partition_mv(0, 0, 16, 16, (-60, 0));
        assert_eq!(st.predict_mv(0, 16, 16, 16), (0, 0));
        let mut st = PictureState::new(&seqh, None, Svq3FrameType::Predicted).unwrap();
        st.store_partition_mv(0, 0, 16, 16, (60, 60));
        // For (16, 0) 16×16: A = (60, 60) only → clamp x to 6·(32−16) −
        // 96 = 0, y to 6·(32−16) − 0 = 96 → (0, 60).
        assert_eq!(st.predict_mv(16, 0, 16, 16), (0, 60));
    }

    /// A reference picture for the P tests: a flat-128 I picture whose
    /// bottom-right macroblock (16…31, 16…31) is lifted by a 16×16 DC
    /// coefficient (nothing is decoded after it, so the DC chain does
    /// not propagate the lift). Returns the lifted value.
    fn reference_with_lifted_mb3(dec: &mut Svq3PictureDecoder) -> u8 {
        // The inter tests read the seam between the lifted macroblock
        // and its flat neighbours, so the edge filter stays off.
        dec.set_options(Svq3DecodeOptions {
            intra_edge_filter: false,
        });
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        p.ue(1);
        p.se(0);
        p.ue(15);
        p.ue(0);
        let au = wire_v1(p.into_bytes());
        let d = dec.decode_access_unit(&au).unwrap();
        assert_eq!(d.frame_type, Svq3FrameType::Intra);
        let lifted = d.picture.luma_sample(16, 16);
        assert_ne!(lifted, 128);
        assert_eq!(d.picture.luma_sample(15, 16), 128);
        assert_eq!(d.picture.luma_sample(16, 15), 128);
        lifted
    }

    #[test]
    fn p_slice_skip_and_inter_macroblocks_predict_from_the_reference() {
        let seqh = seqh_32x32(); // no sub-pel precisions → no selector bits
        let mut dec = Svq3PictureDecoder::new(seqh).unwrap();
        let lifted = reference_with_lifted_mb3(&mut dec);

        let mut p = Packer::new();
        slice_header(&mut p, 0, 13, false);
        // MB0, MB1: skip (flat 128 copies, vectors (0, 0)).
        p.ue(0);
        p.ue(0);
        // MB2 (0, 16): inter 16×16, mvd (dy 0, dx +1). Predictor: no A,
        // B = MB0 and C = MB1 both (0, 0) → (0, 0); full-pel +1 → stored
        // (6, 0): the block reads x = 1…16 of rows 16…31, so its column
        // 15 sees the lifted macroblock and the rest is 128. Inter
        // pattern code 0 → nothing coded.
        p.ue(1);
        p.se(0);
        p.se(1);
        p.ue(0);
        // MB3 (16, 16): inter 8×8 (code 4), four zero mvd pairs. Every
        // partition's median lands on (0, 0) (A = MB2's (6, 0) is
        // outvoted by B / C = (0, 0)), so the lifted block is copied.
        p.ue(4);
        for _ in 0..4 {
            p.se(0);
            p.se(0);
        }
        p.ue(0);
        let au = wire_v1(p.into_bytes());
        let d = dec.decode_access_unit(&au).unwrap();
        assert_eq!(d.frame_type, Svq3FrameType::Predicted);
        let pic = &d.picture;
        for y in 0..16 {
            for x in 0..32 {
                assert_eq!(pic.luma_sample(x, y), 128, "skip row ({x},{y})");
            }
        }
        for y in 16..32 {
            for x in 0..15 {
                assert_eq!(pic.luma_sample(x, y), 128, "MB2 ({x},{y})");
            }
            assert_eq!(pic.luma_sample(15, y), lifted, "MB2 column 15 row {y}");
            for x in 16..32 {
                assert_eq!(pic.luma_sample(x, y), lifted, "MB3 ({x},{y})");
            }
        }
        assert!(pic.cb().iter().all(|&s| s == 128));
        // The P picture is now the reference: a further all-skip P
        // picture reproduces it.
        let mut p = Packer::new();
        slice_header(&mut p, 0, 13, false);
        for _ in 0..4 {
            p.ue(0);
        }
        let au = wire_v1(p.into_bytes());
        let d2 = dec.decode_access_unit(&au).unwrap();
        assert_eq!(d2.picture.luma(), pic.luma());
    }

    #[test]
    fn p_slice_inter_residual_and_delta() {
        let seqh = seqh_32x32();
        let mut dec = Svq3PictureDecoder::new(seqh).unwrap();
        reference_with_lifted_mb3(&mut dec);
        // Delta enable set. MB0: inter 16×16, zero mvd, inter pattern
        // code 2 → luma quadrant 0 only; delta +1 → qp 14; block 0 = +1
        // at the DC (dequant[14] = 19561 → (19561·169 + 0x80000) >> 20
        // = 3), three empty blocks. Then three skips.
        let mut p = Packer::new();
        slice_header(&mut p, 0, 13, true);
        p.ue(1);
        p.se(0);
        p.se(0);
        p.ue(2);
        p.se(1);
        p.ue(1);
        p.ue(0);
        for _ in 0..3 {
            p.ue(0);
        }
        for _ in 0..3 {
            p.ue(0);
        }
        let au = wire_v1(p.into_bytes());
        let d = dec.decode_access_unit(&au).unwrap();
        let pic = &d.picture;
        let expected = crate::svq3_pred::reconstruct_sample(
            128,
            crate::svq3_dequant::finalise_dc(169 * 19561),
        );
        assert_eq!(expected, 131);
        assert_eq!(pic.luma_sample(0, 0), expected);
        assert_eq!(pic.luma_sample(3, 3), expected);
        assert_eq!(pic.luma_sample(4, 0), 128);
        assert_eq!(pic.luma_sample(8, 8), 128);
    }

    #[test]
    fn p_slice_uncoded_tail_is_copied_from_the_reference() {
        // A type-2 P slice covering only macroblock 0; the remaining
        // three are copied from the reference (spec/08 §7).
        let seqh = seqh_32x32();
        let mut dec = Svq3PictureDecoder::new(seqh).unwrap();
        let lifted = reference_with_lifted_mb3(&mut dec);
        let mut p = Packer::new();
        p.ue(0); // P
        p.push(6, 0); // first_mb
        p.push(8, 0);
        p.push(5, 13);
        p.push(1, 0);
        p.push(1, 0);
        p.push(1, 0);
        p.push(2, 0);
        p.push(1, 0);
        // MB0: inter 16×16 with mvd (+1, +1) full-pel → reads
        // (1…16, 1…16): only its sample (15, 15) sees the lifted block.
        p.ue(1);
        p.se(1);
        p.se(1);
        p.ue(0);
        let mut au = wire_v2_slice(p.into_bytes());
        au.push(SVQ3_FRAME_END);
        let d = dec.decode_access_unit(&au).unwrap();
        assert_eq!(d.picture.luma_sample(0, 0), 128);
        assert_eq!(d.picture.luma_sample(14, 15), 128);
        assert_eq!(d.picture.luma_sample(15, 15), lifted);
        // The uncoded tail: MB1 / MB2 flat, MB3 lifted.
        assert_eq!(d.picture.luma_sample(20, 5), 128);
        assert_eq!(d.picture.luma_sample(5, 20), 128);
        assert_eq!(d.picture.luma_sample(20, 20), lifted);
        assert!(d.picture.cb().iter().all(|&s| s == 128));
    }
}
