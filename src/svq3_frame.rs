//! SVQ3 intra access-unit decoder — the slice-level frame walk.
//!
//! Composes the staged parse + arithmetic layers into a whole-picture
//! intra decode: the slice envelope walk (`docs/video/svq3/wiki/
//! Sorenson_Video_3.wiki` §"Slice Header"), the per-macroblock type
//! dispatch (wiki §"Macroblock layer" + the r446 fixture-pinned I-frame
//! wire mapping — see [`crate::svq3_mb::IFrameMbType`]), the intra-4×4
//! mode / CBP / residual grammar (wiki §"Intra macroblock information
//! decoding", `docs/video/svq3/spec/03-coded-block-pattern.md`,
//! `docs/video/svq3/spec/06-residual-coefficient-coding.md`), the
//! intra-16×16 separate-luma-DC grammar
//! (`docs/video/svq3/spec/04-dc-secondary-transform.md` §4), and the
//! per-macroblock reconstruction composition of
//! [`crate::svq3_recon`] / [`crate::svq3_picture`].
//!
//! ## Element order within a macroblock
//!
//! * **Intra 4×4** (I-frame wire code 0): eight prediction-mode pair
//!   codes (wiki §"Intra macroblock information decoding"), the CBP
//!   code number through the **intra** mapping table (spec/03 §2/§4 —
//!   the compressor's intra-4×4 writer emits modes first, then CBP,
//!   then the coefficient streams), the optional quantiser delta
//!   ("before actual coefficient data", gated on the slice header's
//!   delta flag and a non-empty CBP), then the coded luma 4×4 blocks —
//!   quadrant raster order, four blocks per coded quadrant in raster
//!   order (spec/03 §1.1), through the alternate-scan book below
//!   macroblock quantiser 24 and the normal book otherwise (spec/06
//!   §3) — and the chroma section.
//! * **Intra 16×16** (wire codes 1..=24): the optional quantiser delta
//!   (the type always carries the separate DC block), the separate
//!   luma DC block through the normal-zigzag book (spec/04 §4.1 /
//!   §4.4), sixteen luma AC blocks with the scan starting at position
//!   1 when the type's `luma_ac` bit is set (spec/04 §4.3), then the
//!   chroma section per the type's implied chroma class (spec/03 §2 —
//!   no CBP element on the wire).
//! * **Separate-DC-only** (wire code 25): the optional quantiser delta
//!   and the separate luma DC block only (the wiki list's "no other
//!   blocks coded"). Reconstruction uses the 16×16 DC predictor; the
//!   staged docs do not pin this type's predictor, so the choice is
//!   documented as a reading.
//!
//! The **chroma section** decodes per plane (Cb first, then Cr), each
//! plane's 2×2 DC block first and, at chroma class 2, its four 4×4 AC
//! blocks with the scan starting at position 1 — the spec/04 §2.1
//! reconstruction order. The staged docs pin the per-plane
//! reconstruction order but not the cross-plane interleaving; the
//! per-plane reading here is flagged in the crate README's docs-gap
//! list.
//!
//! ## What this module cannot yet do
//!
//! The staged real-stream fixtures (`docs/video/svq3/fixtures/`) begin
//! with a first macroblock whose element sequence consumes far more
//! codes than the grammars above account for (24 universal codes for
//! the all-black first macroblock of the 320×240 fixture, against 2
//! for a spec'd empty 16×16), so end-to-end pixel validation against
//! `expected.yuv` is blocked on that docs gap; every macroblock
//! *after* the first tiles those streams exactly (see the
//! [`crate::svq3_mb::IFrameMbType`] census note). The decoder here is
//! validated against synthetic streams built from the same staged
//! grammar in the crate tests.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::svq3::{
    macroblock_position, mb_grid_dims, num_macroblocks, parse_wire_slice, read_universal_code,
    unpermute_slice_payload, Svq3FrameType, Svq3MacroblockPosition, Svq3SequenceHeader,
    Svq3SliceHeader, SVQ3_FRAME_END,
};
use crate::svq3_cbp::read_cbp_intra;
use crate::svq3_coeff::{
    decode_chroma_dc_2x2, decode_residual_4x4_alt, decode_residual_4x4_normal,
};
use crate::svq3_dequant::{luma_dc_secondary_transform, DEQUANT_COEFF_TABLE_LEN};
use crate::svq3_mb::{
    classify_mb_type, decode_intra_4x4_modes_with_context, IFrameMbType, Intra16x16Params,
    IntraNeighbour, Svq3MbType,
};
use crate::svq3_mv::read_quantiser_delta;
use crate::svq3_picture::{ChromaSelect, Svq3Picture};
use crate::svq3_recon::{
    intra_modes_from_grid, reconstruct_intra_16x16_luma_macroblock_with_dc,
    reconstruct_intra_chroma_plane_from_coeffs,
    reconstruct_intra_luma_macroblock_from_coeffs_intra_dc, ChromaPlane, LumaMacroblock,
    Svq3Luma16x16Mode,
};
use crate::svq3_scan::{ALT_SCAN_4X4_SCAN, ALT_SCAN_QUANTISER_THRESHOLD, NORMAL_ZIGZAG_4X4_SCAN};

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

/// Running state of one intra picture decode.
struct IntraFrameDecoder {
    picture: Svq3Picture,
    mb_cols: u32,
    /// Per-macroblock decoded intra-4×4 mode grids (block-index order);
    /// `None` for 16×16-intra / separate-DC macroblocks. Feeds the
    /// wiki `pred_table` neighbour context of later macroblocks.
    mode_state: Vec<Option<[u8; 16]>>,
}

impl IntraFrameDecoder {
    fn new(mb_cols: u32, mb_rows: u32) -> Self {
        Self {
            picture: Svq3Picture::new(mb_cols as usize, mb_rows as usize),
            mb_cols,
            mode_state: vec![None; (mb_cols * mb_rows) as usize],
        }
    }

    /// The wiki `pred_table` context rows for the current macroblock's
    /// top-row / left-column sub-blocks: the actual decoded mode when
    /// the neighbour macroblock is 4×4-intra-coded, the "value 2"
    /// class when it exists but is not, `-1`/Outside when there is no
    /// neighbour.
    fn edge_contexts(
        &self,
        mb_index: usize,
        pos: Svq3MacroblockPosition,
    ) -> ([IntraNeighbour; 4], [IntraNeighbour; 4]) {
        let cols = self.mb_cols as usize;
        let mut top = [IntraNeighbour::Outside; 4];
        let mut left = [IntraNeighbour::Outside; 4];
        if pos.top_available {
            top = match self.mode_state[mb_index - cols] {
                // Bottom row of the macroblock above: block indices 12..=15.
                Some(grid) => [
                    IntraNeighbour::Mode4x4(grid[12]),
                    IntraNeighbour::Mode4x4(grid[13]),
                    IntraNeighbour::Mode4x4(grid[14]),
                    IntraNeighbour::Mode4x4(grid[15]),
                ],
                None => [IntraNeighbour::Intra16x16OrInter; 4],
            };
        }
        if pos.left_available {
            left = match self.mode_state[mb_index - 1] {
                // Rightmost column of the macroblock to the left:
                // block indices 3, 7, 11, 15.
                Some(grid) => [
                    IntraNeighbour::Mode4x4(grid[3]),
                    IntraNeighbour::Mode4x4(grid[7]),
                    IntraNeighbour::Mode4x4(grid[11]),
                    IntraNeighbour::Mode4x4(grid[15]),
                ],
                None => [IntraNeighbour::Intra16x16OrInter; 4],
            };
        }
        (top, left)
    }

    /// Apply a decoded quantiser delta to the running macroblock
    /// quantiser, rejecting results outside the dequantisation-ladder
    /// domain.
    fn apply_quantiser_delta(br: &mut BitReader<'_>, qp: &mut u32) -> Result<()> {
        let delta = read_quantiser_delta(br)?;
        let next = *qp as i64 + delta as i64;
        if next < 0 || next >= DEQUANT_COEFF_TABLE_LEN as i64 {
            return Err(Error::InvalidQuantiser(next as i32));
        }
        *qp = next as u32;
        Ok(())
    }

    /// Decode the chroma section for chroma class `class` (0/1/2):
    /// per plane, the 2×2 DC block then (class 2 only) the four AC
    /// blocks with the scan starting at position 1.
    fn decode_chroma_section(br: &mut BitReader<'_>, class: u8) -> Result<MbChromaCoeffs> {
        let mut out = MbChromaCoeffs::zero();
        if class == 0 {
            return Ok(out);
        }
        for (dc, ac) in [
            (&mut out.cb_dc, &mut out.cb_ac),
            (&mut out.cr_dc, &mut out.cr_ac),
        ] {
            decode_chroma_dc_2x2(br, dc)?;
            if class >= 2 {
                for block in ac.iter_mut() {
                    decode_residual_4x4_normal(br, &NORMAL_ZIGZAG_4X4_SCAN, 1, block)?;
                }
            }
        }
        Ok(out)
    }

    /// Reconstruct both chroma planes into the picture canvas.
    fn reconstruct_chroma(
        &mut self,
        pos: Svq3MacroblockPosition,
        qp: u32,
        chroma: &MbChromaCoeffs,
    ) {
        for (which, dc, ac) in [
            (ChromaSelect::Cb, chroma.cb_dc, &chroma.cb_ac),
            (ChromaSelect::Cr, chroma.cr_dc, &chroma.cr_ac),
        ] {
            let mut plane = ChromaPlane::new();
            self.picture.bind_chroma_neighbours(pos, which, &mut plane);
            reconstruct_intra_chroma_plane_from_coeffs(&mut plane, dc, ac, qp);
            self.picture.blit_chroma(pos, which, &plane);
        }
    }

    /// Decode + reconstruct one intra macroblock from the slice bit
    /// stream. `qp` is the running macroblock quantiser (mutated by
    /// the per-macroblock delta when `delta_qp_present`).
    fn decode_macroblock(
        &mut self,
        br: &mut BitReader<'_>,
        mb_index: usize,
        qp: &mut u32,
        delta_qp_present: bool,
    ) -> Result<()> {
        let pos = macroblock_position(mb_index as u32, self.mb_cols)?;
        let mb_type = match classify_mb_type(Svq3FrameType::Intra, read_universal_code(br)?)? {
            Svq3MbType::IIntra(t) => t,
            // classify_mb_type(Intra, _) only produces IIntra.
            _ => return Err(Error::ReconstructFailed),
        };

        match mb_type {
            IFrameMbType::Intra4x4 => {
                self.decode_intra_4x4_mb(br, mb_index, pos, qp, delta_qp_present)
            }
            IFrameMbType::Intra16x16(params) => {
                self.mode_state[mb_index] = None;
                self.decode_intra_16x16_mb(br, pos, qp, delta_qp_present, params)
            }
            IFrameMbType::SeparateDcOnly => {
                self.mode_state[mb_index] = None;
                // The wiki list's "luma DCs coded in a separate 4×4
                // block and no other blocks coded": grammar = the DC
                // block only; predictor not pinned — decoded as the
                // 16×16 DC-predictor reading (pred selector 2).
                self.decode_intra_16x16_mb(
                    br,
                    pos,
                    qp,
                    delta_qp_present,
                    Intra16x16Params {
                        pred_mode: 2,
                        cbp_chroma: 0,
                        luma_ac: false,
                    },
                )
            }
        }
    }

    /// The intra-4×4 macroblock body: modes, CBP, optional quantiser
    /// delta, coded luma blocks, chroma section, reconstruction.
    fn decode_intra_4x4_mb(
        &mut self,
        br: &mut BitReader<'_>,
        mb_index: usize,
        pos: Svq3MacroblockPosition,
        qp: &mut u32,
        delta_qp_present: bool,
    ) -> Result<()> {
        let (top_ctx, left_ctx) = self.edge_contexts(mb_index, pos);
        let grid = decode_intra_4x4_modes_with_context(br, top_ctx, left_ctx)?;
        let modes = intra_modes_from_grid(&grid)?;
        self.mode_state[mb_index] = Some(*grid.modes());

        let cbp = read_cbp_intra(br)?;
        if delta_qp_present && cbp.value() != 0 {
            Self::apply_quantiser_delta(br, qp)?;
        }

        // Coded luma blocks: quadrant raster order (spec/03 §1.1), the
        // four 4×4 blocks of a coded quadrant in raster order; scan /
        // book selected on the macroblock quantiser (spec/06 §3).
        let mut coeff_blocks = [[0i32; 16]; 16];
        let use_alt = *qp < ALT_SCAN_QUANTISER_THRESHOLD;
        for quadrant in 0..4usize {
            if !cbp.luma_quadrant_coded(quadrant) {
                continue;
            }
            let q_row = quadrant / 2;
            let q_col = quadrant % 2;
            for sub in 0..4usize {
                let block_row = q_row * 2 + sub / 2;
                let block_col = q_col * 2 + sub % 2;
                let block_index = block_row * 4 + block_col;
                if use_alt {
                    decode_residual_4x4_alt(
                        br,
                        &ALT_SCAN_4X4_SCAN,
                        &mut coeff_blocks[block_index],
                    )?;
                } else {
                    decode_residual_4x4_normal(
                        br,
                        &NORMAL_ZIGZAG_4X4_SCAN,
                        0,
                        &mut coeff_blocks[block_index],
                    )?;
                }
            }
        }

        let chroma_class = if cbp.chroma_ac_coded() {
            2
        } else if cbp.chroma_dc_coded() {
            1
        } else {
            0
        };
        let chroma = Self::decode_chroma_section(br, chroma_class)?;

        // Reconstruction: the wiki §"Macroblock transform and
        // dequantization" intra-luma inline-DC scale path.
        let mut mb = LumaMacroblock::new();
        self.picture.bind_luma_neighbours(pos, &mut mb);
        reconstruct_intra_luma_macroblock_from_coeffs_intra_dc(&mut mb, &modes, &coeff_blocks, *qp);
        self.picture.blit_luma(pos, &mb);
        self.reconstruct_chroma(pos, *qp, &chroma);
        Ok(())
    }

    /// The intra-16×16 macroblock body: optional quantiser delta,
    /// separate luma DC block, per-type luma AC + implied chroma,
    /// reconstruction.
    fn decode_intra_16x16_mb(
        &mut self,
        br: &mut BitReader<'_>,
        pos: Svq3MacroblockPosition,
        qp: &mut u32,
        delta_qp_present: bool,
        params: Intra16x16Params,
    ) -> Result<()> {
        // The type always carries the separate DC block, i.e.
        // coefficient data, so the delta (when the slice signals
        // deltas may be present) precedes it.
        if delta_qp_present {
            Self::apply_quantiser_delta(br, qp)?;
        }

        // The separate luma DC block: sixteen coefficient positions
        // through the ordinary normal-zigzag residual decoder
        // (spec/04 §4.1 step 1 / §4.4 — never the alternate scan).
        let mut dc_block = [0i32; 16];
        decode_residual_4x4_normal(br, &NORMAL_ZIGZAG_4X4_SCAN, 0, &mut dc_block)?;
        let dc_terms = luma_dc_secondary_transform(*qp, dc_block);

        // Sixteen luma AC blocks in raster order, scan starting at
        // position 1, present only when the type's luma_ac bit is set
        // (spec/04 §4.3).
        let mut ac_blocks = [[0i32; 16]; 16];
        if params.luma_ac {
            for block in ac_blocks.iter_mut() {
                decode_residual_4x4_normal(br, &NORMAL_ZIGZAG_4X4_SCAN, 1, block)?;
            }
        }

        let chroma = Self::decode_chroma_section(br, params.cbp_chroma)?;

        let mut mb = LumaMacroblock::new();
        self.picture.bind_luma_neighbours(pos, &mut mb);
        let mode = Svq3Luma16x16Mode::from_pred_mode(
            params.pred_mode,
            pos.top_available,
            pos.left_available,
        );
        reconstruct_intra_16x16_luma_macroblock_with_dc(&mut mb, mode, &ac_blocks, &dc_terms, *qp);
        self.picture.blit_luma(pos, &mb);
        self.reconstruct_chroma(pos, *qp, &chroma);
        Ok(())
    }
}

/// Decode one SVQ3 **intra** access unit (all slices of one I-frame)
/// into a reconstructed picture.
///
/// `au` is the access-unit byte stream: one or more wire slices
/// (1-byte version/size-size prefix + 1–3-byte size + permuted body
/// each), optionally terminated by the `0xFF` frame-end sentinel.
/// `seqh` is the stream's parsed `SEQH` sequence header (dimensions +
/// protection flag).
///
/// Slices are decoded in order; a version-2 slice's macroblock-offset
/// field must equal the running macroblock cursor (there is no
/// skip-ahead in an intra picture). Returns the fully reconstructed
/// [`Svq3Picture`] once every macroblock of the grid is decoded.
///
/// Returns [`Error::NotImplemented`] for a P/B slice,
/// [`Error::Truncated`] when the access unit ends before the
/// macroblock grid is complete, and the per-macroblock structural
/// errors of the layers this walk composes.
// internal — exposed for tests/fuzz; not part of the stable API
#[doc(hidden)]
pub fn decode_intra_access_unit(seqh: &Svq3SequenceHeader, au: &[u8]) -> Result<Svq3Picture> {
    let (mb_cols, mb_rows) = mb_grid_dims(seqh);
    let total_mbs = num_macroblocks(seqh) as usize;
    if mb_cols == 0 || mb_rows == 0 {
        return Err(Error::BadBitWidth(0));
    }
    let mut dec = IntraFrameDecoder::new(mb_cols, mb_rows);
    let mut mb_cursor = 0usize;
    let mut offset = 0usize;

    while mb_cursor < total_mbs {
        if offset >= au.len() || au[offset] == SVQ3_FRAME_END {
            // The access unit ended before the macroblock grid was
            // complete.
            return Err(Error::Truncated);
        }
        let (header, _) = parse_wire_slice(&au[offset..], total_mbs as u32, seqh.protected)?;
        if header.frame_type != Svq3FrameType::Intra {
            return Err(Error::NotImplemented);
        }
        let sss = header.slice_size_size as usize;
        let body_start = offset + 1 + sss;
        let body_end = body_start + header.slice_size as usize;
        // parse_wire_slice validated the bounds.
        let body = &au[body_start..body_end];
        let unpermuted = unpermute_slice_payload(body, header.slice_size_size)?;

        mb_cursor = decode_slice_macroblocks(&mut dec, &header, &unpermuted, mb_cursor, total_mbs)?;
        offset = body_end;
    }

    Ok(dec.picture)
}

/// Decode the macroblocks one unpermuted slice body carries, returning
/// the advanced macroblock cursor.
fn decode_slice_macroblocks(
    dec: &mut IntraFrameDecoder,
    header: &Svq3SliceHeader,
    unpermuted: &[u8],
    mut mb_cursor: usize,
    total_mbs: usize,
) -> Result<usize> {
    if let Some(mb_offset) = header.mb_offset_v2 {
        // Version-2 slices carry their starting macroblock offset;
        // an intra picture decodes every macroblock in raster order,
        // so the offset must equal the running cursor.
        if mb_offset as usize != mb_cursor {
            return Err(Error::InvalidFrameCode(mb_offset));
        }
    }

    let mut br = BitReader::new(unpermuted);
    for _ in 0..header.header_end_bit {
        br.read_bit()?;
    }

    let mut qp = u32::from(header.slice_qp);
    // A multi-slice picture's non-final slices end where their payload
    // runs out; the last (or only) slice must carry the rest of the
    // picture. A slice boundary is only legal at a macroblock
    // boundary, so a Truncated error from the very first read of a
    // macroblock hands the walk to the next slice rather than failing
    // the picture — for version-1 slices that signal "has more
    // slices", and for version-2 slices generally (the next slice's
    // macroblock offset re-validates continuity above).
    let more_slices = header.has_more_slices_v1 == Some(true) || header.mb_offset_v2.is_some();
    while mb_cursor < total_mbs {
        let mb_start_bits = br.bits_consumed();
        match dec.decode_macroblock(&mut br, mb_cursor, &mut qp, header.delta_qp_present) {
            Ok(()) => mb_cursor += 1,
            Err(Error::Truncated)
                if more_slices && br.bits_consumed().saturating_sub(mb_start_bits) < 16 =>
            {
                // Out of payload at (or right after) a macroblock
                // boundary — continue in the next slice.
                return Ok(mb_cursor);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(mb_cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svq3::parse_extradata;

    /// Pack `(width, value)` items into bytes, MSB-first.
    struct Packer {
        bits: Vec<u8>,
    }

    impl Packer {
        fn new() -> Self {
            Self { bits: Vec::new() }
        }

        fn push(&mut self, width: u32, value: u32) {
            assert!((1..=32).contains(&width));
            assert!(width == 32 || value < (1u32 << width));
            for i in (0..width).rev() {
                self.bits.push(((value >> i) & 1) as u8);
            }
        }

        /// Append one universal-code codeword for code number `n`
        /// (spec/06 §1 interleaved layout).
        fn ue(&mut self, n: u32) {
            let exp = 31 - (n + 1).leading_zeros();
            let data = n + 1 - (1u32 << exp);
            match exp {
                0 => self.push(1, 1),
                1 => self.push(3, 0b010 | data),
                _ => {
                    self.push(1, 0);
                    self.push(1, 0);
                    self.push(1, (data >> (exp - 1)) & 1);
                    self.push(1, (data >> (exp - 2)) & 1);
                    for i in (0..exp - 2).rev() {
                        self.push(1, 0);
                        self.push(1, (data >> i) & 1);
                    }
                    self.push(1, 1);
                }
            }
        }

        /// Append the signed universal code for `v` (spec/06 §1.1).
        fn se(&mut self, v: i32) {
            let code = if v == 0 {
                0
            } else if v > 0 {
                (v as u32) * 2 - 1
            } else {
                (-v as u32) * 2
            };
            self.ue(code);
        }

        fn into_bytes(self) -> Vec<u8> {
            let mut out = vec![0u8; self.bits.len().div_ceil(8)];
            for (i, &b) in self.bits.iter().enumerate() {
                out[i / 8] |= b << (7 - (i % 8));
            }
            out
        }
    }

    /// A 32×32 (2×2 macroblock) SEQH via the explicit-dimension escape.
    fn seqh_32x32() -> Svq3SequenceHeader {
        let mut p = Packer::new();
        p.push(3, 7); // frame_size_code = 7 → explicit dims
        p.push(12, 32); // width
        p.push(12, 32); // height
        p.push(1, 0); // halfpel
        p.push(1, 0); // thirdpel
        p.push(4, 0); // unknown
        p.push(1, 1); // no B frames
        p.push(1, 0); // no optional data
        p.push(1, 0); // not protected
        let payload = p.into_bytes();
        let mut extradata = Vec::new();
        extradata.extend_from_slice(b"SEQH");
        extradata.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        extradata.extend_from_slice(&payload);
        parse_extradata(&extradata).unwrap()
    }

    /// Start a version-1 intra slice-header bit stream: frame code I,
    /// no more slices, frame number 0, the given quantiser, delta flag,
    /// unknown 0, no optional data.
    fn intra_slice_header(p: &mut Packer, qp: u32, delta_qp: bool) {
        p.ue(2); // frame code 2 = I
        p.push(1, 0); // v1: no more slices
        p.push(8, 0); // frame number
        p.push(5, qp);
        p.push(1, u32::from(delta_qp));
        p.push(1, 0); // unknown
        p.push(1, 0); // optional-data loop: stop
    }

    /// Wrap a packed slice payload in the version-1 wire envelope
    /// (prefix byte, 2-byte size, permuted body) + frame-end sentinel.
    fn wire_v1(payload: Vec<u8>) -> Vec<u8> {
        let sss = 2u8;
        let mut au = Vec::new();
        au.push((sss << 5) | 1); // slice_size_size = 2, version 1
        au.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        // unpermute moves the trailing (sss - 1) bytes to the front, so
        // the wire form moves the leading byte to the back.
        au.extend_from_slice(&payload[1..]);
        au.push(payload[0]);
        au.push(SVQ3_FRAME_END);
        au
    }

    /// One all-empty intra-4×4 macroblock: type 0, eight pair codes 0,
    /// CBP code 3 (pattern 0) — the 14-bit unit the staged 320×240
    /// fixture's black I-frame tiles 299 times.
    fn push_empty_i4_mb(p: &mut Packer) {
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        p.ue(3);
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
        // No residual anywhere: luma = the DC predictor chain seeded by
        // the top-left macroblock's no-neighbour fallback (128), chroma
        // = the 128 chroma DC fallback.
        assert!(pic.luma().iter().all(|&s| s == 128));
        assert!(pic.cb().iter().all(|&s| s == 128));
        assert!(pic.cr().iter().all(|&s| s == 128));
    }

    #[test]
    fn dc_only_16x16_frame_reconstructs_uniform_shift() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        // MB0: type code 3 → 16×16 params idx 2 (pred DC, no chroma,
        // no luma AC); DC block = one coefficient — normal-book code 15
        // = level +3 run 0 per the staged tables/05 — then end-of-block.
        p.ue(3);
        p.ue(15);
        p.ue(0);
        // MBs 1..=3: empty intra 4×4.
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();

        // Independently compose the expected MB0 plane: DC level +3 at
        // qp 13 through the spec/04 §4 pipeline over a flat-128
        // prediction.
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
        // Chroma is untouched everywhere.
        assert!(pic.cb().iter().all(|&s| s == 128));
    }

    #[test]
    fn chroma_dc_class_shifts_chroma_plane() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        // MB0: type code 7 → idx 6 = pred 2 (DC), chroma class 1, no
        // luma AC. Empty luma DC block; each chroma plane's 2×2 DC
        // block carries one level — chroma-book code 7 = level +3 run 0
        // per the staged tables/05 — then its end-of-block.
        p.ue(7);
        p.ue(0); // luma DC EOB
        p.ue(7); // Cb DC level +3
        p.ue(0); // Cb DC EOB
        p.ue(7); // Cr DC level +3
        p.ue(0); // Cr DC EOB
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();

        // Expected chroma: prediction 128 + the spec/04 §2 pipeline for
        // levels [3, 0, 0, 0] at qp 13 (chroma remap applied inside).
        let b = crate::svq3_dequant::dequantize_chroma_dc_levels(13, [3, 0, 0, 0]);
        let expected =
            crate::svq3_pred::reconstruct_sample(128, crate::svq3_dequant::finalise_dc(169 * b[0]));
        assert_ne!(
            expected, 128,
            "test premise: the chroma shift must be visible"
        );
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(pic.cb_sample(x, y), expected, "cb ({x},{y})");
                assert_eq!(pic.cr_sample(x, y), expected, "cr ({x},{y})");
            }
        }
        // Luma of MB0 stays at the flat fallback.
        assert_eq!(pic.luma_sample(0, 0), 128);
    }

    #[test]
    fn intra4x4_inline_dc_uses_fixed_scale() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 30, false); // qp 30 ≥ 24 → normal scan
                                               // MB0: intra 4×4, all-zero mode pairs, CBP code 0 → pattern 47
                                               // (all luma quadrants + chroma DC and AC). Every luma block
                                               // carries one inline DC: level +1 run 0 (code 1), then its
                                               // end-of-block.
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        p.ue(0); // CBP code 0 → 47
        for _ in 0..16 {
            p.ue(1); // luma block: +1 at scan position 0 (the inline DC)
            p.ue(0); // end of block
        }
        // chroma class 2: per plane DC block then 4 AC blocks.
        for _ in 0..2 {
            p.ue(0); // DC EOB
            for _ in 0..4 {
                p.ue(0); // AC EOB
            }
        }
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();

        // Inline intra DC of +1 at ANY quantiser: the wiki fixed scale
        // 13·13·1538 → residual (169·1538 + 0x80000) >> 20 = 0, so the
        // plane stays at the prediction. The test pins that the DC did
        // NOT run through the general coeff·dequant[30] scale, which
        // would visibly shift the block (126635·169 >> 20 = 20).
        assert_eq!(pic.luma_sample(0, 0), 128);
    }

    #[test]
    fn quantiser_delta_applies_and_bounds() {
        let seqh = seqh_32x32();
        // Delta flag set; 16×16 macroblock carries a delta of +4;
        // subsequent MBs see the updated quantiser.
        let mut p = Packer::new();
        intra_slice_header(&mut p, 10, true);
        p.ue(3); // 16×16, pred DC, no chroma, no AC
        p.se(4); // quantiser delta +4 → qp 14
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

        // A delta that drives the quantiser out of 0..=31 is rejected.
        let mut p = Packer::new();
        intra_slice_header(&mut p, 30, true);
        p.ue(3);
        p.se(4); // 30 + 4 = 34 → out of range
        let au = wire_v1(p.into_bytes());
        assert_eq!(
            decode_intra_access_unit(&seqh, &au).unwrap_err(),
            Error::InvalidQuantiser(34)
        );
    }

    #[test]
    fn intra4x4_empty_cbp_skips_delta() {
        let seqh = seqh_32x32();
        // Delta flag set but the intra-4×4 macroblock's CBP is 0 → no
        // delta element on the wire (the very next macroblock must
        // parse cleanly).
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, true);
        for _ in 0..4 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        assert!(pic.luma().iter().all(|&s| s == 128));
    }

    #[test]
    fn separate_dc_only_type_decodes() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        p.ue(25); // separate-DC-only type
        p.ue(15); // DC level +3
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
        assert_eq!(pic.luma_sample(0, 0), expected);
    }

    #[test]
    fn intra4x4_mode_context_crosses_macroblocks() {
        // A frame of four all-empty intra-4×4 macroblocks: every
        // macroblock after the first resolves its edge sub-blocks'
        // pred_table context from the NEIGHBOUR macroblock's decoded
        // mode grid (all-DC ⇒ Mode4x4(2), lookup row 3) rather than
        // the coarse availability classes — the walk must thread the
        // grids and still resolve every pair (a broken context would
        // hit a -1 sentinel or desynchronise).
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        for _ in 0..4 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        assert!(pic.luma().iter().all(|&s| s == 128));
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
    }

    #[test]
    fn p_slice_is_not_implemented() {
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        p.ue(0); // frame code 0 = P
        p.push(1, 0);
        p.push(8, 0);
        p.push(5, 13);
        p.push(1, 0);
        p.push(1, 0);
        p.push(1, 0);
        let au = wire_v1(p.into_bytes());
        assert_eq!(
            decode_intra_access_unit(&seqh, &au).unwrap_err(),
            Error::NotImplemented
        );
    }

    #[test]
    fn bitflip_and_truncation_error_cleanly() {
        // Robustness: every byte-truncation of a valid AU and a sweep
        // of single-bit flips either decodes or errors — no panics.
        let seqh = seqh_32x32();
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        p.ue(3);
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
        // MB0: type code 15 → idx 14 = pred 2 (DC), chroma 0,
        // luma_ac = 1. DC block: level +3 run 0 (code 15), EOB. Then
        // sixteen AC blocks, scan start 1: block 0 carries level +1
        // run 0 (lands at scan position 1 = raster 1), the other
        // fifteen are empty.
        p.ue(15);
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

        // Component composition of the same macroblock: no neighbours,
        // DC fallback prediction, spec/04 §4 DC scatter + the placed
        // AC coefficient in block 0.
        let mut dc_block = [0i32; 16];
        dc_block[0] = 3;
        let v = luma_dc_secondary_transform(13, dc_block);
        let mut ac_blocks = [[0i32; 16]; 16];
        ac_blocks[0][NORMAL_ZIGZAG_4X4_SCAN[1]] = 1;
        let mut mb = crate::svq3_recon::LumaMacroblock::new();
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
        // The AC coefficient must be visible (the walk read it with
        // scan start 1, so it cannot have been swallowed as a DC).
        assert_ne!(pic.luma_sample(1, 0), pic.luma_sample(0, 0));
    }

    /// Start a version-2 intra slice-header bit stream with the given
    /// macroblock offset (6-bit field for a 4-macroblock picture:
    /// `max(ceil_log2(4), 6) = 6`).
    fn intra_slice_header_v2(p: &mut Packer, mb_offset: u32, qp: u32) {
        p.ue(2); // frame code 2 = I
        p.push(6, mb_offset);
        p.push(8, 0); // frame number
        p.push(5, qp);
        p.push(1, 0); // delta qp flag
        p.push(1, 0); // unknown
        p.push(1, 0); // optional-data loop stop
    }

    /// Wrap a packed slice payload in the version-2 wire envelope
    /// (no trailing frame-end byte — the caller concatenates).
    fn wire_v2_slice(payload: Vec<u8>) -> Vec<u8> {
        let sss = 2u8;
        let mut out = Vec::new();
        out.push((sss << 5) | 2); // slice_size_size = 2, version 2
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&payload[1..]);
        out.push(payload[0]);
        out
    }

    #[test]
    fn v2_two_slice_access_unit_decodes() {
        let seqh = seqh_32x32();
        // Slice 0: macroblocks 0..2 at offset 0; slice 1: macroblocks
        // 2..4 at offset 2. Each slice's payload ends exactly at its
        // last macroblock (plus byte padding), exercising the
        // continuation-at-truncation rule and the offset check.
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

        // A wrong second-slice offset is rejected.
        let mut p0 = Packer::new();
        intra_slice_header_v2(&mut p0, 0, 13);
        push_empty_i4_mb(&mut p0);
        push_empty_i4_mb(&mut p0);
        let mut p1 = Packer::new();
        intra_slice_header_v2(&mut p1, 3, 13);
        push_empty_i4_mb(&mut p1);
        push_empty_i4_mb(&mut p1);
        let mut au = wire_v2_slice(p0.into_bytes());
        au.extend_from_slice(&wire_v2_slice(p1.into_bytes()));
        au.push(SVQ3_FRAME_END);
        assert_eq!(
            decode_intra_access_unit(&seqh, &au).unwrap_err(),
            Error::InvalidFrameCode(3)
        );
    }

    #[test]
    fn intra4x4_alt_scan_block_matches_component_composition() {
        let seqh = seqh_32x32();
        // qp 13 < 24 → the intra-4×4 luma blocks use the alternate
        // scan. MB0: all-DC modes, CBP code 2 → pattern 15 (all four
        // luma quadrants, no chroma); every block's two half-scans:
        // the first half carries level +2 run 0 (alt-book code 5 →
        // the inline DC, zero residual under the fixed intra scale),
        // the second half is empty.
        let mut p = Packer::new();
        intra_slice_header(&mut p, 13, false);
        p.ue(0);
        for _ in 0..8 {
            p.ue(0);
        }
        p.ue(2); // CBP code 2 → intra pattern 15
        for _ in 0..16 {
            p.ue(5); // half 1: +2 at alt scan position 0 (raster 0)
            p.ue(0); // half 1 end
            p.ue(0); // half 2 end
        }
        for _ in 0..3 {
            push_empty_i4_mb(&mut p);
        }
        let au = wire_v1(p.into_bytes());
        let pic = decode_intra_access_unit(&seqh, &au).unwrap();
        // Inline DC level +2 under the fixed 13·13·1538 scale rounds
        // to zero residual, so the macroblock stays at the DC
        // prediction chain — but the stream MUST have consumed the 48
        // block codes (a desync would corrupt the following
        // macroblocks or fail the decode).
        assert!(pic.luma().iter().all(|&s| s == 128));
    }
}
