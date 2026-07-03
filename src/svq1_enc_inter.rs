//! SVQ1 inter-frame (P) encoder — per-macroblock SKIP / INTER /
//! INTRA mode decision with motion search.
//!
//! Wire shape per macroblock (wiki §"Decoding Interframe Plane Data" /
//! spec/02 §2.5.1): the T03 coding-mode codeword, then for INTER one
//! differential motion vector (`dx` then `dy`, each a single signed
//! T02 codeword at alphabet position `component + 32` — spec/06
//! §6.2.3 Reading B as pinned by the black-box P-frame fixture), then
//! the breadth-first leaf walk on the INTER tables over the
//! motion-compensated baseline ([`crate::svq1_enc_tree`], spec/04
//! §4.4 / §4.6.2).
//!
//! ## Mode decision
//!
//! Per macroblock the encoder evaluates
//!
//! * **SKIP** — the reference block verbatim (`mv = (0, 0)` at the
//!   same position, spec/04 §4.6.4); costs one T03 codeword.
//! * **INTER** — motion search (below) + λ-cost leaf tree on the
//!   inter half with per-leaf SKIP allowed (spec/04 §4.5.5).
//! * **INTRA** — the intra-half λ-tree over a zero predictor
//!   (spec/04 §4.4.3), exactly as an I-frame macroblock.
//!
//! and commits the candidate minimising `SSE + λ · bits`. The MV
//! cache ([`crate::svq1_mv_cache::Svq1MvCache`]) is updated on commit
//! with the decoder's own store rules (spec/06 §6.8.1), so subsequent
//! macroblocks' median predictors see exactly what the decoder will
//! see.
//!
//! ## Motion search
//!
//! Two-phase per macroblock: a full-pel SAD scan of radius
//! [`Svq1InterParams::search_radius`] centred on the median
//! predictor (plus the all-zero candidate), then a ±1 half-pel
//! refinement around the full-pel winner. Every candidate must be
//! reachable on the wire: both the final MV and the differential
//! `mv − predictor` are confined to `[-32, +31]` (spec/06 §6.6 and
//! T02's 64-position alphabet), so `clip(predictor + d) == mv` holds
//! and the decoder reproduces the choice exactly.
//!
//! ## Frame assembly
//!
//! The P-frame header is the spec/01 layout with the I-frame-only
//! field group absent: `frame_code` (22 bits, `0x20`), temporal
//! reference (8), picture type `1` (2), `checksum_present = 0`,
//! `unknown_flag_1 = 0` — 34 bits, then the Y / U / V plane payloads
//! bit-tight (spec/02 §2.4 / §2.5). The returned reconstruction is
//! produced by OUR OWN decoder run over the emitted bytes, so it is
//! the authoritative reference picture for the next frame in the
//! chain.

use crate::error::{Error, Result};
use crate::svq1_codebook::SVQ1_VLC_MB_MODE;
use crate::svq1_codebook::SVQ1_VLC_MV_COMPONENT;
use crate::svq1_enc::{BitWriter, Svq1PlaneRef};
use crate::svq1_enc_tree::{emit_macroblock, plan_macroblock, MbPlan};
use crate::svq1_mc::{motion_compensate_block, Svq1ReferencePlane, MC_BLOCK_DIM};
use crate::svq1_motion_predictor::{predict, Svq1Mv, MV_COMPONENT_MAX, MV_COMPONENT_MIN};
use crate::svq1_mv_cache::Svq1MvCache;
use crate::svq1_plane::{chroma_dim, decode_frame, Svq1DecodedFrame, Svq1PlaneCanvas, MB_DIM};
use crate::svq1_vlc::Svq1Half;

/// P-frame encoder tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Svq1InterParams {
    /// Rate weight (SSE units per wire bit) shared by the mode
    /// decision and the leaf-tree search.
    pub lambda: u64,
    /// Full-pel motion-search radius around the median predictor
    /// (candidates stay inside the `[-32, +31]` half-pel MV and
    /// differential domains regardless).
    pub search_radius: u8,
    /// 8-bit temporal reference emitted in the frame header.
    pub temporal_reference: u8,
}

impl Default for Svq1InterParams {
    fn default() -> Self {
        Self {
            lambda: 32,
            search_radius: 8,
            temporal_reference: 0,
        }
    }
}

/// An encoded P-frame: the codec bytes plus the decoder-authoritative
/// reconstruction (what any conforming decoder produces from `bytes`
/// against the same reference — use it as the next frame's reference).
#[derive(Debug, Clone)]
pub struct Svq1EncodedFrame {
    /// The complete codec-frame byte array.
    pub bytes: Vec<u8>,
    /// Our decoder's reconstruction of `bytes`.
    pub reconstruction: Svq1DecodedFrame,
}

/// T03 alphabet positions for the four macroblock modes (the
/// permutation pinned by the black-box P-frame fixture — see
/// [`crate::svq1_plane::read_mb_mode`]).
const T03_POS_SKIP: usize = 3;
const T03_POS_INTER: usize = 0;
const T03_POS_INTRA: usize = 2;

/// Bit length of the T03 codeword at `position`.
fn mb_mode_bits(position: usize) -> u32 {
    u32::from(SVQ1_VLC_MB_MODE.0[position].1)
}

/// Bit length of one T02 MV-component codeword for differential `d`.
fn mv_component_bits(d: i32) -> u32 {
    u32::from(SVQ1_VLC_MV_COMPONENT.0[(d + 32) as usize].1)
}

/// Emit one differential MV: `dx` then `dy`, each one signed T02
/// codeword at position `component + 32` (spec/06 §6.2.3 Reading B).
fn push_mv_differential(w: &mut BitWriter, dx: i32, dy: i32) {
    w.push_code(&SVQ1_VLC_MV_COMPONENT.0, (dx + 32) as usize);
    w.push_code(&SVQ1_VLC_MV_COMPONENT.0, (dy + 32) as usize);
}

/// Assemble the motion-compensated 16×16 baseline for a macroblock
/// whose top-left 8×8 slot is `(block_row, block_col)`, with the
/// single MV `mv` applied to all four sub-blocks (the INTER shape of
/// spec/04 §4.6.2).
fn mc_macroblock(
    reference: &Svq1ReferencePlane<'_>,
    block_row: usize,
    block_col: usize,
    mv: Svq1Mv,
) -> [u8; 256] {
    let mut out = [0u8; 256];
    for (sub_row, sub_col) in [(0usize, 0usize), (0, 1), (1, 0), (1, 1)] {
        let patch = motion_compensate_block(
            reference,
            ((block_col + sub_col) * MC_BLOCK_DIM) as i32,
            ((block_row + sub_row) * MC_BLOCK_DIM) as i32,
            mv,
        );
        for row in 0..MC_BLOCK_DIM {
            let dst = (sub_row * 8 + row) * 16 + sub_col * 8;
            out[dst..dst + MC_BLOCK_DIM]
                .copy_from_slice(&patch[row * MC_BLOCK_DIM..(row + 1) * MC_BLOCK_DIM]);
        }
    }
    out
}

/// Sum of absolute differences of a 16×16 candidate against the
/// target (the cheap phase-1 motion metric).
fn sad(target: &[u8; 256], candidate: &[u8; 256]) -> u64 {
    target
        .iter()
        .zip(candidate.iter())
        .map(|(&t, &c)| u64::from(t.abs_diff(c)))
        .sum()
}

/// Both components of `mv` and of `mv − predictor` must sit in the
/// `[-32, +31]` wire domain (spec/06 §6.6; T02's 64 positions).
fn mv_is_codable(mv: Svq1Mv, predictor: Svq1Mv) -> bool {
    let ok = |c: i32| (MV_COMPONENT_MIN..=MV_COMPONENT_MAX).contains(&c);
    ok(mv.x) && ok(mv.y) && ok(mv.x - predictor.x) && ok(mv.y - predictor.y)
}

/// Visible-reference window constraint for one macroblock's motion
/// candidates.
///
/// spec/06 §6.7 leaves the out-of-frame reference extension
/// decoder-implementation-defined (edge replication is only the
/// de-facto convention), and spec/04 §4.7.3 likewise leaves the
/// STORED content of overhang macroblock regions to the
/// implementation. Black-box probing confirms decoders genuinely
/// diverge there: streams whose visible output depends on either
/// behaviour reconstruct differently across decoders. A portable
/// encoder therefore only emits MVs for which every VISIBLE output
/// sample reads exclusively VISIBLE reference samples — including
/// the second tap of the half-pel interpolator (spec/06 §6.5.1):
/// output position `c` with component `m` reads integer positions
/// `c + ⌊m / 2⌋` and (when `m` is odd) `c + ⌈m / 2⌉`.
///
/// The zero vector is always permitted (a macroblock's visible
/// region reads itself).
#[derive(Debug, Clone, Copy)]
pub(crate) struct MvVisibleWindow {
    /// MB top-left in plane coordinates.
    x0: i32,
    y0: i32,
    /// Last VISIBLE output position covered by this MB.
    xlast: i32,
    ylast: i32,
    /// Visible plane dimensions.
    vw: i32,
    vh: i32,
}

impl MvVisibleWindow {
    /// Window for the macroblock at `(mb_x, mb_y)` of a
    /// `visible_w × visible_h` plane.
    pub(crate) fn new(mb_x: usize, mb_y: usize, visible_w: usize, visible_h: usize) -> Self {
        let x0 = (mb_x * MB_DIM) as i32;
        let y0 = (mb_y * MB_DIM) as i32;
        let vw = visible_w as i32;
        let vh = visible_h as i32;
        Self {
            x0,
            y0,
            xlast: (x0 + MB_DIM as i32 - 1).min(vw - 1),
            ylast: (y0 + MB_DIM as i32 - 1).min(vh - 1),
            vw,
            vh,
        }
    }

    /// `true` when every visible output of the MB reads only visible
    /// reference samples under `mv`.
    pub(crate) fn permits(&self, mv: Svq1Mv) -> bool {
        self.x0 + (mv.x >> 1) >= 0
            && self.y0 + (mv.y >> 1) >= 0
            && self.xlast + ((mv.x + 1) >> 1) < self.vw
            && self.ylast + ((mv.y + 1) >> 1) < self.vh
    }
}

/// Two-phase motion search for one macroblock: full-pel SAD scan of
/// radius `radius` centred on the median `predictor` (plus the zero
/// vector), then ±1 half-pel refinement. Returns the best codable MV
/// inside the visible-reference `window`.
pub(crate) fn search_motion_vector(
    reference: &Svq1ReferencePlane<'_>,
    target: &[u8; 256],
    block_row: usize,
    block_col: usize,
    predictor: Svq1Mv,
    radius: u8,
    window: MvVisibleWindow,
) -> Svq1Mv {
    let mut best = Svq1Mv::ZERO;
    let mut best_sad = u64::MAX;
    let consider = |mv: Svq1Mv, best: &mut Svq1Mv, best_sad: &mut u64| {
        if !mv_is_codable(mv, predictor) || !window.permits(mv) {
            return;
        }
        let candidate = mc_macroblock(reference, block_row, block_col, mv);
        let s = sad(target, &candidate);
        if s < *best_sad {
            *best_sad = s;
            *best = mv;
        }
    };

    // Phase 1: full-pel scan around the predictor's full-pel centre,
    // with the zero vector always in the candidate set (it is codable
    // whenever the predictor itself is in range).
    consider(Svq1Mv::ZERO, &mut best, &mut best_sad);
    let radius = i32::from(radius);
    let centre_x = predictor.x >> 1;
    let centre_y = predictor.y >> 1;
    for fy in -radius..=radius {
        for fx in -radius..=radius {
            let mv = Svq1Mv::new((centre_x + fx) * 2, (centre_y + fy) * 2);
            consider(mv, &mut best, &mut best_sad);
        }
    }

    // Phase 2: ±1 half-pel refinement around the phase-1 winner.
    let centre = best;
    for hy in -1..=1i32 {
        for hx in -1..=1i32 {
            if hx == 0 && hy == 0 {
                continue;
            }
            let mv = Svq1Mv::new(centre.x + hx, centre.y + hy);
            consider(mv, &mut best, &mut best_sad);
        }
    }
    best
}

/// One decided macroblock candidate.
struct MbCandidate {
    /// T03 alphabet position to emit.
    t03_position: usize,
    /// Differential MV to emit (INTER only).
    mv_diff: Option<(i32, i32)>,
    /// Leaf tree to emit (None for MB-level SKIP).
    plan: Option<MbPlan>,
    /// Half the plan's leaves are coded on.
    half: Svq1Half,
    /// Total wire bits (T03 + MV + tree).
    bits: u32,
    /// SSE against the source target.
    sse: u64,
}

impl MbCandidate {
    fn cost(&self, lambda: u64) -> u64 {
        self.sse
            .saturating_add(lambda.saturating_mul(u64::from(self.bits)))
    }
}

/// Encode one interframe plane payload against `reference` (the
/// previous I- or P-frame's reconstructed canvas for the same
/// plane): per macroblock in raster order, evaluate SKIP / INTER /
/// INTRA, emit the winner, and update the MV cache exactly as the
/// decoder will (spec/06 §6.8.1).
fn encode_inter_plane(
    w: &mut BitWriter,
    src: &Svq1PlaneRef<'_>,
    reference: &Svq1PlaneCanvas,
    params: &Svq1InterParams,
) -> Result<()> {
    let mb_cols = src.width.div_ceil(MB_DIM);
    let mb_rows = src.height.div_ceil(MB_DIM);
    if reference.stride != mb_cols * MB_DIM || reference.rows != mb_rows * MB_DIM {
        return Err(Error::MissingReference);
    }
    let ref_plane = Svq1ReferencePlane::new(&reference.samples, reference.stride, reference.rows)
        .ok_or(Error::MissingReference)?;
    let mut cache = Svq1MvCache::new(mb_cols, mb_rows);

    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let (block_row, block_col) = (mb_y * 2, mb_x * 2);
            let block = src.block(mb_x * MB_DIM, mb_y * MB_DIM, MB_DIM, MB_DIM);
            let mut target = [0u8; 256];
            target.copy_from_slice(&block);

            // SKIP: reference macroblock verbatim.
            let mut skip_recon = [0u8; 256];
            for row in 0..MB_DIM {
                let start = (mb_y * MB_DIM + row) * reference.stride + mb_x * MB_DIM;
                skip_recon[row * 16..row * 16 + 16]
                    .copy_from_slice(&reference.samples[start..start + MB_DIM]);
            }
            let skip = MbCandidate {
                t03_position: T03_POS_SKIP,
                mv_diff: None,
                plan: None,
                half: Svq1Half::Inter,
                bits: mb_mode_bits(T03_POS_SKIP),
                sse: sad_sq(&target, &skip_recon),
            };

            // INTER: motion search + inter-half leaf tree.
            let predictor = predict(cache.inter_neighbours(block_row, block_col));
            let window = MvVisibleWindow::new(mb_x, mb_y, src.width, src.height);
            let mv = search_motion_vector(
                &ref_plane,
                &target,
                block_row,
                block_col,
                predictor,
                params.search_radius,
                window,
            );
            let (dx, dy) = (mv.x - predictor.x, mv.y - predictor.y);
            let mc_baseline = mc_macroblock(&ref_plane, block_row, block_col, mv);
            let inter_plan =
                plan_macroblock(&target, &mc_baseline, Svq1Half::Inter, true, params.lambda);
            let inter = MbCandidate {
                t03_position: T03_POS_INTER,
                mv_diff: Some((dx, dy)),
                bits: mb_mode_bits(T03_POS_INTER)
                    + mv_component_bits(dx)
                    + mv_component_bits(dy)
                    + inter_plan.bits,
                sse: inter_plan.sse,
                plan: Some(inter_plan),
                half: Svq1Half::Inter,
            };

            // INTRA: intra-half leaf tree over a zero predictor.
            let intra_plan =
                plan_macroblock(&target, &[0u8; 256], Svq1Half::Intra, false, params.lambda);
            let intra = MbCandidate {
                t03_position: T03_POS_INTRA,
                mv_diff: None,
                bits: mb_mode_bits(T03_POS_INTRA) + intra_plan.bits,
                sse: intra_plan.sse,
                plan: Some(intra_plan),
                half: Svq1Half::Intra,
            };

            // Commit the λ-cost winner (bits break ties).
            let winner = [skip, inter, intra]
                .into_iter()
                .min_by(|a, b| {
                    a.cost(params.lambda)
                        .cmp(&b.cost(params.lambda))
                        .then(a.bits.cmp(&b.bits))
                })
                .expect("three candidates");

            w.push_code(&SVQ1_VLC_MB_MODE.0, winner.t03_position);
            match winner.t03_position {
                T03_POS_SKIP | T03_POS_INTRA => {
                    cache.store_skip_intra(block_row, block_col);
                }
                _ => {
                    let (dx, dy) = winner.mv_diff.expect("INTER carries an MV");
                    push_mv_differential(w, dx, dy);
                    let stored = cache.decode_inter(block_row, block_col, dx, dy);
                    debug_assert_eq!(stored, mv, "cache reproduces the searched MV");
                }
            }
            if let Some(plan) = &winner.plan {
                emit_macroblock(w, plan, winner.half);
            }
        }
    }
    Ok(())
}

/// SSE between two 16×16 blocks.
fn sad_sq(a: &[u8; 256], b: &[u8; 256]) -> u64 {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| {
            let d = i64::from(x) - i64::from(y);
            (d * d) as u64
        })
        .sum()
}

/// Encode one SVQ1 P-frame from tightly-packed YUV 4:1:0 planes
/// against the previous frame's reconstruction.
///
/// `reference` must be the DECODED form of the previously-emitted
/// frame (the [`Svq1EncodedFrame::reconstruction`] of the previous
/// call, or the decode of an intra frame) — predicting from raw
/// source planes instead would drift from what decoders see.
/// Dimensions must match the reference's.
pub fn encode_inter_frame(
    y: Svq1PlaneRef<'_>,
    u: Svq1PlaneRef<'_>,
    v: Svq1PlaneRef<'_>,
    reference: &Svq1DecodedFrame,
    params: &Svq1InterParams,
) -> Result<Svq1EncodedFrame> {
    let (width, height) = (reference.width(), reference.height());
    if y.width != width
        || y.height != height
        || y.samples.len() != width * height
        || u.width != chroma_dim(width)
        || u.height != chroma_dim(height)
        || u.samples.len() != u.width * u.height
        || v.width != u.width
        || v.height != u.height
        || v.samples.len() != v.width * v.height
    {
        return Err(Error::BadBitWidth(0));
    }

    let mut w = BitWriter::new();
    // P-frame header (spec/01): the I-frame-only field group is
    // absent; 22 + 8 + 2 + 1 + 1 = 34 bits.
    w.push_bits(22, 0x20); // frame code
    w.push_bits(8, u32::from(params.temporal_reference));
    w.push_bits(2, 1); // picture type = P
    w.push_bits(1, 0); // checksum_present
    w.push_bits(1, 0); // unknown_flag_1

    encode_inter_plane(&mut w, &y, &reference.y, params)?;
    encode_inter_plane(&mut w, &u, &reference.u, params)?;
    encode_inter_plane(&mut w, &v, &reference.v, params)?;

    let bytes = w.into_bytes();
    let reconstruction = decode_frame(&bytes, Some(reference))?;
    Ok(Svq1EncodedFrame {
        bytes,
        reconstruction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svq1_enc::{encode_intra_frame, Svq1EncoderMode};
    use crate::svq1_plane::decode_intra_frame;

    const W: usize = 64;
    const H: usize = 48;

    fn gradient_plane(width: usize, height: usize, seed: u32) -> Vec<u8> {
        (0..width * height)
            .map(|i| {
                let x = (i % width) as u32;
                let y = (i / width) as u32;
                ((x * 3 + y * 5 + seed) % 256) as u8
            })
            .collect()
    }

    /// Shift a plane `shift` samples right with edge replication —
    /// pure horizontal motion the INTER search should find.
    fn shifted(plane: &[u8], width: usize, height: usize, shift: usize) -> Vec<u8> {
        let mut out = vec![0u8; plane.len()];
        for row in 0..height {
            for col in 0..width {
                let src_col = col.saturating_sub(shift);
                out[row * width + col] = plane[row * width + src_col];
            }
        }
        out
    }

    fn plane_refs<'a>(
        y: &'a [u8],
        u: &'a [u8],
        v: &'a [u8],
    ) -> (Svq1PlaneRef<'a>, Svq1PlaneRef<'a>, Svq1PlaneRef<'a>) {
        let (cw, ch) = (chroma_dim(W), chroma_dim(H));
        (
            Svq1PlaneRef {
                samples: y,
                width: W,
                height: H,
            },
            Svq1PlaneRef {
                samples: u,
                width: cw,
                height: ch,
            },
            Svq1PlaneRef {
                samples: v,
                width: cw,
                height: ch,
            },
        )
    }

    fn intra_reference(y: &[u8], u: &[u8], v: &[u8]) -> Svq1DecodedFrame {
        let (yr, ur, vr) = plane_refs(y, u, v);
        let bytes = encode_intra_frame(yr, ur, vr, Svq1EncoderMode::Adaptive { lambda: 16 })
            .expect("intra encodes");
        decode_intra_frame(&bytes).expect("intra decodes")
    }

    /// An identical source frame after the reference is all-SKIP: the
    /// P-frame reconstruction equals the reference and the stream is
    /// tiny (one T03 codeword per macroblock).
    #[test]
    fn identical_frame_encodes_as_all_skip() {
        let (cw, ch) = (chroma_dim(W), chroma_dim(H));
        let y = gradient_plane(W, H, 7);
        let u = gradient_plane(cw, ch, 101);
        let v = gradient_plane(cw, ch, 202);
        let reference = intra_reference(&y, &u, &v);

        // Source = the reference's own reconstruction (what a static
        // scene looks like to the encoder).
        let ry = reference.y.visible();
        let ru = reference.u.visible();
        let rv = reference.v.visible();
        let (yr, ur, vr) = plane_refs(&ry, &ru, &rv);
        let encoded =
            encode_inter_frame(yr, ur, vr, &reference, &Svq1InterParams::default()).expect("P");

        assert!(!encoded.reconstruction.header.is_intra());
        assert_eq!(encoded.reconstruction.y.visible(), ry, "Y plane");
        assert_eq!(encoded.reconstruction.u.visible(), ru, "U plane");
        assert_eq!(encoded.reconstruction.v.visible(), rv, "V plane");
        // 12 + 3 + 3 macroblocks, ~1 bit each, plus the 34-bit header.
        assert!(
            encoded.bytes.len() <= 8,
            "all-SKIP stream should be tiny, got {} bytes",
            encoded.bytes.len()
        );
    }

    /// Pure horizontal motion: the P-frame must code dramatically
    /// cheaper than an intra frame of the same content, and the
    /// reconstruction must beat the SKIP-everything baseline.
    #[test]
    fn translated_frame_uses_motion_compensation() {
        let (cw, ch) = (chroma_dim(W), chroma_dim(H));
        let y0 = gradient_plane(W, H, 7);
        let u0 = gradient_plane(cw, ch, 101);
        let v0 = gradient_plane(cw, ch, 202);
        let reference = intra_reference(&y0, &u0, &v0);

        let y1 = shifted(&reference.y.visible(), W, H, 3);
        let u1 = reference.u.visible();
        let v1 = reference.v.visible();
        let (yr, ur, vr) = plane_refs(&y1, &u1, &v1);

        let params = Svq1InterParams {
            lambda: 16,
            ..Default::default()
        };
        let p = encode_inter_frame(yr, ur, vr, &reference, &params).expect("P encodes");
        let intra = encode_intra_frame(yr, ur, vr, Svq1EncoderMode::Adaptive { lambda: 16 })
            .expect("intra encodes");
        assert!(
            p.bytes.len() * 2 < intra.len(),
            "P-frame ({}) should be far cheaper than intra ({})",
            p.bytes.len(),
            intra.len()
        );

        let sse = |got: &[u8], want: &[u8]| -> u64 {
            got.iter()
                .zip(want.iter())
                .map(|(&a, &b)| {
                    let d = i64::from(a) - i64::from(b);
                    (d * d) as u64
                })
                .sum()
        };
        let skip_sse = sse(&reference.y.visible(), &y1);
        let p_sse = sse(&p.reconstruction.y.visible(), &y1);
        assert!(
            p_sse < skip_sse / 4,
            "MC must beat the static baseline ({p_sse} vs {skip_sse})"
        );
    }

    /// Chained P-frames stay decodable and drift-free: each frame's
    /// reconstruction (returned by the encoder) is byte-identical to
    /// an independent decode of the emitted bytes.
    #[test]
    fn three_frame_chain_round_trips() {
        let (cw, ch) = (chroma_dim(W), chroma_dim(H));
        let y0 = gradient_plane(W, H, 7);
        let u0 = gradient_plane(cw, ch, 101);
        let v0 = gradient_plane(cw, ch, 202);
        let mut reference = intra_reference(&y0, &u0, &v0);

        for shift in [2usize, 5] {
            let y = shifted(&reference.y.visible(), W, H, shift);
            let u = reference.u.visible();
            let v = reference.v.visible();
            let (yr, ur, vr) = plane_refs(&y, &u, &v);
            let p = encode_inter_frame(yr, ur, vr, &reference, &Svq1InterParams::default())
                .expect("P encodes");
            let independent = decode_frame(&p.bytes, Some(&reference)).expect("P decodes");
            assert_eq!(
                independent.y.visible(),
                p.reconstruction.y.visible(),
                "shift {shift} Y"
            );
            assert_eq!(
                independent.u.visible(),
                p.reconstruction.u.visible(),
                "shift {shift} U"
            );
            assert_eq!(
                independent.v.visible(),
                p.reconstruction.v.visible(),
                "shift {shift} V"
            );
            reference = p.reconstruction;
        }
    }

    /// Mis-sized planes are rejected.
    #[test]
    fn rejects_mis_sized_planes() {
        let (cw, ch) = (chroma_dim(W), chroma_dim(H));
        let y = gradient_plane(W, H, 7);
        let u = gradient_plane(cw, ch, 101);
        let v = gradient_plane(cw, ch, 202);
        let reference = intra_reference(&y, &u, &v);
        let (yr, ur, _) = plane_refs(&y, &u, &v);
        let bad_v = Svq1PlaneRef {
            samples: &v[..v.len() - 1],
            width: cw,
            height: ch,
        };
        assert!(
            encode_inter_frame(yr, ur, bad_v, &reference, &Svq1InterParams::default()).is_err()
        );
    }
}
