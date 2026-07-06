//! SVQ1 plane decode — the per-plane macroblock walk that turns the
//! post-header bitstream into reconstructed sample planes.
//!
//! Composes the layers the earlier rounds staged into the actual
//! wire decode of `docs/video/svq1/spec/02-bitstream-organisation.md`
//! (three bit-concatenated plane payloads, per-plane 16 × 16
//! macroblock raster scan) + `spec/03-block-hierarchy.md` (the
//! breadth-first L=5 → L=0 subdivision walk) +
//! `spec/04-multistage-vq-decoder.md` (per-leaf stage-count dispatch,
//! codebook stage accumulation) + `spec/05-mean-removal.md` (the
//! per-leaf mean VLC) — driven by the sixteen [`crate::svq1_vlc`]
//! wire tables.
//!
//! ## Canvas geometry
//!
//! Each plane decodes into a [`Svq1PlaneCanvas`] padded up to whole
//! macroblocks (`ceil(dim / 16) × 16` per spec/02 §2.3): overhang
//! macroblocks decode in full and the overhang samples are simply
//! never exported (spec/04 §4.7.3 — "the overhang positions are
//! still written by the decoder … the container clips them at
//! display"). [`Svq1PlaneCanvas::visible`] crops back to the
//! declared plane dimensions.
//!
//! ## Intra frame assembly
//!
//! [`decode_intra_frame`] walks the whole codec frame: header
//! (`crate::parse_frame_header`), then the Y / U / V plane payloads
//! back-to-back with NO padding or realignment between them
//! (spec/02 §2.2.1) at the YUV 4:1:0 chroma geometry (`W/4 × H/4`
//! per spec/02 §2.2). The result carries all three canvases plus
//! the parsed header.

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::header::{parse_frame_header, Svq1FrameHeader, Svq1PictureType};
use crate::svq1_blocktree::{read_block_decision, subdivide, Svq1BlockDecision, Svq1Level};
use crate::svq1_codebook::codebook_half;
use crate::svq1_mc::{motion_compensate_block, Svq1ReferencePlane, MC_BLOCK_DIM};
use crate::svq1_motion_predictor::Svq1Mv;
use crate::svq1_mv_cache::Svq1MvCache;
use crate::svq1_reconstruct::{reconstruct_leaf, LeafStage};
use crate::svq1_stage_indices::read_stage_indices;
use crate::svq1_vlc::{read_intra_mean, read_stage_count, Svq1Half};

/// Macroblock (level-5 block) edge length in samples, per
/// `docs/video/svq1/spec/02-bitstream-organisation.md` §2.3.
pub const MB_DIM: usize = 16;

/// A decoded sample plane padded up to whole macroblocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svq1PlaneCanvas {
    /// Declared (visible) plane width in samples.
    pub width: usize,
    /// Declared (visible) plane height in samples.
    pub height: usize,
    /// Padded row length — `ceil(width / 16) * 16`.
    pub stride: usize,
    /// Padded row count — `ceil(height / 16) * 16`.
    pub rows: usize,
    /// Row-major padded samples (`stride × rows`).
    pub samples: Vec<u8>,
}

impl Svq1PlaneCanvas {
    /// Allocate a zeroed canvas for a `width × height` plane.
    pub fn new(width: usize, height: usize) -> Self {
        let stride = width.div_ceil(MB_DIM) * MB_DIM;
        let rows = height.div_ceil(MB_DIM) * MB_DIM;
        Self {
            width,
            height,
            stride,
            rows,
            samples: vec![0u8; stride * rows],
        }
    }

    /// Macroblock grid dimensions `(mb_cols, mb_rows)` per spec/02
    /// §2.3 (`ceil(dim / 16)`).
    pub fn mb_grid(&self) -> (usize, usize) {
        (self.stride / MB_DIM, self.rows / MB_DIM)
    }

    /// Crop the padded canvas back to the visible `width × height`
    /// region (row-major, tightly packed).
    pub fn visible(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width * self.height);
        for row in 0..self.height {
            let start = row * self.stride;
            out.extend_from_slice(&self.samples[start..start + self.width]);
        }
        out
    }
}

/// Position + level of one block inside the plane canvas during the
/// breadth-first walk. `x` / `y` are absolute canvas sample
/// coordinates of the block's top-left corner.
#[derive(Debug, Clone, Copy)]
struct BlockGeom {
    x: usize,
    y: usize,
    level: Svq1Level,
}

/// Split a block into its two children per the spec/03 §3.4
/// dimension-halving table: L=5 → top + bottom L=4; L=4 → left +
/// right L=3; L=3 → top + bottom L=2; L=2 → left + right L=1;
/// L=1 → top + bottom L=0. The FIRST child is the top / left half
/// (§3.4.1 queue-insertion order).
fn halve(geom: BlockGeom) -> (BlockGeom, BlockGeom) {
    let (child, _) = subdivide(geom.level).expect("halve called on L=0");
    let (w, h) = geom.level.block_dims();
    let (w, h) = (w as usize, h as usize);
    let (first, second) = if w >= h {
        // Wider-than-tall (or the square L=5 / L=3 tie, which the
        // spec resolves as "halve H" — top + bottom): the table
        // splits 16×16 and 8×8 horizontally, 16×8 and 8×4
        // vertically, 4×4 horizontally. Squares halve H; strictly
        // wider blocks halve W.
        if w == h {
            (
                BlockGeom {
                    x: geom.x,
                    y: geom.y,
                    level: child,
                },
                BlockGeom {
                    x: geom.x,
                    y: geom.y + h / 2,
                    level: child,
                },
            )
        } else {
            (
                BlockGeom {
                    x: geom.x,
                    y: geom.y,
                    level: child,
                },
                BlockGeom {
                    x: geom.x + w / 2,
                    y: geom.y,
                    level: child,
                },
            )
        }
    } else {
        (
            BlockGeom {
                x: geom.x,
                y: geom.y,
                level: child,
            },
            BlockGeom {
                x: geom.x,
                y: geom.y + h / 2,
                level: child,
            },
        )
    };
    (first, second)
}

/// Read the current canvas content of `geom`'s block in raster order
/// — the per-leaf predictor per spec/04 §4.6: all-zero for an intra
/// macroblock (whose canvas region was cleared at MB entry), the
/// motion-compensated reference for an inter macroblock (whose
/// canvas region was MC-filled before the leaf walk).
fn read_predictor(canvas: &Svq1PlaneCanvas, geom: BlockGeom) -> Vec<u8> {
    let (w, h) = geom.level.block_dims();
    let (w, h) = (w as usize, h as usize);
    let mut out = Vec::with_capacity(w * h);
    for row in 0..h {
        let start = (geom.y + row) * canvas.stride + geom.x;
        out.extend_from_slice(&canvas.samples[start..start + w]);
    }
    out
}

/// Write a reconstructed leaf back into the canvas in raster order
/// (spec/04 §4.7).
fn write_leaf(canvas: &mut Svq1PlaneCanvas, geom: BlockGeom, samples: &[u8]) {
    let (w, h) = geom.level.block_dims();
    let (w, h) = (w as usize, h as usize);
    debug_assert_eq!(samples.len(), w * h);
    for row in 0..h {
        let start = (geom.y + row) * canvas.stride + geom.x;
        canvas.samples[start..start + w].copy_from_slice(&samples[row * w..(row + 1) * w]);
    }
}

/// Decode one leaf block: stage-count VLC → (SKIP | mean | mean +
/// stages) → writeback, per spec/03 §3.6 + spec/04 §4.1..§4.7 +
/// spec/05 §5.3.
///
/// The predictor is the canvas's CURRENT content at the leaf
/// position (zero for intra MBs whose region was cleared at entry;
/// the motion-compensated reference for inter MBs). SKIP (`N = −1`)
/// leaves that content untouched — which on the intra path is a
/// stream-format violation surfaced as
/// [`Error::UnexpectedIntraSkip`] (spec/04 §4.9.1).
fn decode_leaf(
    br: &mut BitReader<'_>,
    canvas: &mut Svq1PlaneCanvas,
    geom: BlockGeom,
    half: Svq1Half,
) -> Result<()> {
    let n = read_stage_count(br, geom.level, half)?;
    if n == -1 {
        return match half {
            Svq1Half::Intra => Err(Error::UnexpectedIntraSkip),
            // Inter-path SKIP: the block stays at its
            // motion-compensated reference content (spec/04 §4.5.5).
            Svq1Half::Inter => Ok(()),
        };
    }
    let mean: i16 = match half {
        Svq1Half::Intra => i16::from(read_intra_mean(br)?),
        Svq1Half::Inter => crate::svq1_vlc::read_inter_mean(br)?,
    };

    if n == 0 {
        // Mean-only leaf (spec/04 §4.5.4) — representable at EVERY
        // level, including the codebook-less L=4 / L=5 (spec/03
        // §3.8.2; spec/04 §4.9.2; pinned empirically by the
        // conformance fixture — real intra streams carry mean-only
        // 16×16 leaves for flat regions): every sample becomes
        // `saturate_u8(predictor + mean)` with no codebook lookup.
        let mut samples = read_predictor(canvas, geom);
        for s in &mut samples {
            *s = (i16::from(*s) + mean).clamp(0, 255) as u8;
        }
        write_leaf(canvas, geom, &samples);
        return Ok(());
    }

    // The wiki gate: `(stages > 0) && (level >= 4)` — no codebook
    // exists at L=4 / L=5 (spec/04 §4.1.2).
    if geom.level.rejects_in_place_quantise() {
        return Err(Error::InvalidLevelQuantise(geom.level));
    }

    let indices = read_stage_indices(br, n as usize)?;
    let mut stages = [LeafStage {
        stage: 1,
        vec_idx: 0,
    }; 6];
    for (i, &vec_idx) in indices.indices().iter().enumerate() {
        stages[i] = LeafStage {
            stage: i + 1,
            vec_idx: usize::from(vec_idx),
        };
    }
    let stages = &stages[..indices.len()];

    let page = codebook_half(geom.level, half).ok_or(Error::InvalidLevelQuantise(geom.level))?;
    let predictor = read_predictor(canvas, geom);
    let reconstructed = reconstruct_leaf(geom.level, page, &predictor, mean, stages)
        .map_err(|_| Error::ReconstructFailed)?;
    write_leaf(canvas, geom, &reconstructed);
    Ok(())
}

/// Decode one macroblock's block tree (breadth-first, L=5 → L=0 per
/// spec/03 §3.5) with `half` selecting the VLC family + codebook
/// half. The MB's canvas region must already hold the per-leaf
/// predictor baseline (zeros for intra; the MC reference for inter).
pub(crate) fn decode_mb_block_tree(
    br: &mut BitReader<'_>,
    canvas: &mut Svq1PlaneCanvas,
    mb_x: usize,
    mb_y: usize,
    half: Svq1Half,
) -> Result<()> {
    // One FIFO queue per level, 5 down to 0 (spec/03 §3.5.1).
    let mut queues: [Vec<BlockGeom>; 6] = Default::default();
    queues[5].push(BlockGeom {
        x: mb_x * MB_DIM,
        y: mb_y * MB_DIM,
        level: Svq1Level::L5,
    });
    for level_idx in (0..6).rev() {
        // Levels drain strictly top-down; a subdivision pushes into
        // the (level − 1) queue which is processed next.
        let queue = std::mem::take(&mut queues[level_idx]);
        for geom in queue {
            match read_block_decision(geom.level, br)? {
                Svq1BlockDecision::Subdivide => {
                    let (first, second) = halve(geom);
                    queues[level_idx - 1].push(first);
                    queues[level_idx - 1].push(second);
                }
                Svq1BlockDecision::Quantise => {
                    decode_leaf(br, canvas, geom, half)?;
                }
            }
        }
    }
    Ok(())
}

/// Zero one macroblock's canvas region — the intra predictor
/// baseline of spec/04 §4.6.1.
fn clear_mb(canvas: &mut Svq1PlaneCanvas, mb_x: usize, mb_y: usize) {
    for row in 0..MB_DIM {
        let start = (mb_y * MB_DIM + row) * canvas.stride + mb_x * MB_DIM;
        canvas.samples[start..start + MB_DIM].fill(0);
    }
}

/// Decode one INTRAFRAME plane payload: every macroblock in raster
/// order (spec/02 §2.4), each through the breadth-first block tree
/// with the intra VLC family + intra codebook half + zero predictor.
pub fn decode_intra_plane(
    br: &mut BitReader<'_>,
    width: usize,
    height: usize,
) -> Result<Svq1PlaneCanvas> {
    let mut canvas = Svq1PlaneCanvas::new(width, height);
    let (mb_cols, mb_rows) = canvas.mb_grid();
    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            clear_mb(&mut canvas, mb_x, mb_y);
            decode_mb_block_tree(br, &mut canvas, mb_x, mb_y, Svq1Half::Intra)?;
        }
    }
    Ok(canvas)
}

/// A fully-decoded SVQ1 frame: the parsed header plus the three
/// YUV 4:1:0 plane canvases (Y at `width × height`; U and V at
/// `ceil(width / 4) × ceil(height / 4)` per spec/02 §2.2).
#[derive(Debug, Clone)]
pub struct Svq1DecodedFrame {
    /// The parsed frame header.
    pub header: Svq1FrameHeader,
    /// Luma plane canvas.
    pub y: Svq1PlaneCanvas,
    /// First chroma plane canvas (quarter resolution each way).
    pub u: Svq1PlaneCanvas,
    /// Second chroma plane canvas.
    pub v: Svq1PlaneCanvas,
}

impl Svq1DecodedFrame {
    /// Frame width in luma samples.
    pub fn width(&self) -> usize {
        self.y.width
    }

    /// Frame height in luma samples.
    pub fn height(&self) -> usize {
        self.y.height
    }
}

/// Chroma plane dimension for a luma dimension — the YUV 4:1:0
/// quarter-in-each-direction subsample of spec/02 §2.2 (`W/4`,
/// rounded up for non-multiple-of-4 luma dimensions).
pub const fn chroma_dim(luma_dim: usize) -> usize {
    luma_dim.div_ceil(4)
}

#[cfg(feature = "registry")]
impl Svq1DecodedFrame {
    /// Bridge the decoded frame to an [`oxideav_core::VideoFrame`]
    /// in `Yuv420P` layout.
    ///
    /// SVQ1's native lattice is YUV 4:1:0 (one chroma sample per
    /// 4×4 luma block, spec/02 §2.2); the framework's `PixelFormat`
    /// enum does not yet carry a 4:1:0 layout, so the bridge
    /// nearest-neighbour-doubles each chroma sample onto the 4:2:0
    /// grid (each 4:1:0 sample covers a 2×2 block of 4:2:0 samples)
    /// cropped to `ceil(W/2) × ceil(H/2)`. The native 4:1:0 planes
    /// stay available on this struct (`y` / `u` / `v`) for callers
    /// that want the unresampled data.
    pub fn to_video_frame_420(&self, pts: Option<i64>) -> oxideav_core::VideoFrame {
        let cw = self.width().div_ceil(2);
        let ch = self.height().div_ceil(2);
        let upsample = |plane: &Svq1PlaneCanvas| -> Vec<u8> {
            let mut out = Vec::with_capacity(cw * ch);
            for row in 0..ch {
                let src_row = (row / 2).min(plane.height.saturating_sub(1));
                for col in 0..cw {
                    let src_col = (col / 2).min(plane.width.saturating_sub(1));
                    out.push(plane.samples[src_row * plane.stride + src_col]);
                }
            }
            out
        };
        oxideav_core::VideoFrame {
            pts,
            planes: vec![
                oxideav_core::VideoPlane {
                    stride: self.width(),
                    data: self.y.visible(),
                },
                oxideav_core::VideoPlane {
                    stride: cw,
                    data: upsample(&self.u),
                },
                oxideav_core::VideoPlane {
                    stride: cw,
                    data: upsample(&self.v),
                },
            ],
        }
    }
}

/// Decode a complete SVQ1 INTRAFRAME chunk: header + Y + U + V.
///
/// Returns [`Error::NotImplemented`] for P / B frames (the inter
/// plane walk needs the reference frame — see
/// [`crate::svq1_frame`]-level decode in the registry layer).
pub fn decode_intra_frame(bytes: &[u8]) -> Result<Svq1DecodedFrame> {
    let header = parse_frame_header(bytes)?;
    if header.picture_type != Svq1PictureType::Intra {
        return Err(Error::NotImplemented);
    }
    let width = usize::from(header.width.ok_or(Error::Truncated)?);
    let height = usize::from(header.height.ok_or(Error::Truncated)?);

    let mut br = BitReader::new(bytes);
    // Re-position after the header (parse_frame_header consumed its
    // own reader; skip the same bit count on ours).
    for _ in 0..header.header_end_bit {
        br.read_bit()?;
    }

    let y = decode_intra_plane(&mut br, width, height)?;
    let u = decode_intra_plane(&mut br, chroma_dim(width), chroma_dim(height))?;
    let v = decode_intra_plane(&mut br, chroma_dim(width), chroma_dim(height))?;

    Ok(Svq1DecodedFrame { header, y, u, v })
}

// ---- Interframe (P / B) decode ------------------------------------------

/// Interframe macroblock coding mode, per
/// `docs/video/svq1/spec/02-bitstream-organisation.md` §2.5.1 /
/// wiki §"Decoding Interframe Plane Data".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq1MbMode {
    /// Block unchanged from the reference frame.
    Skip,
    /// One MV for the whole 16×16 block, then the intra-style leaf
    /// walk on the inter tables over the MC'd baseline.
    Inter,
    /// Four MVs (one per 8×8 sub-block), then the leaf walk.
    Inter4mv,
    /// Intra-coded macroblock inside a P / B frame.
    Intra,
}

/// Read the interframe MB-coding-mode VLC (T03) and map the decoded
/// alphabet position to its semantic mode.
///
/// audit/01 §7.1 left the position → mode permutation open (the
/// shortest, 1-bit codeword sits at position 3, which under the
/// wiki's `0 SKIP / 1 INTER / 2 INTER_4MV / 3 INTRA` numbering would
/// make INTRA — not SKIP — the cheapest mode). The black-box P-frame
/// conformance fixture (`tests/svq1_inter_conformance.rs`) pins the
/// answer as audit/01 §3.2 hypothesis (a) — the alphabet IS permuted
/// against the wiki numbering, by a rotation that puts SKIP on the
/// 1-bit codeword:
///
/// | Position | Code | Mode |
/// |----------|------|------|
/// | 3 | `1`   | SKIP |
/// | 0 | `01`  | INTER |
/// | 1 | `001` | INTER_4MV |
/// | 2 | `000` | INTRA |
///
/// (equivalently `wiki_mode = (position + 1) mod 4`) — the code
/// lengths then rank the modes in the natural motion-video
/// frequency order SKIP > INTER > INTER_4MV ≈ INTRA.
pub fn read_mb_mode(br: &mut BitReader<'_>) -> Result<Svq1MbMode> {
    Ok(match crate::svq1_vlc::read_mb_mode_position(br)? {
        3 => Svq1MbMode::Skip,
        0 => Svq1MbMode::Inter,
        1 => Svq1MbMode::Inter4mv,
        _ => Svq1MbMode::Intra,
    })
}

/// Read one differential motion-vector COMPONENT off the wire.
///
/// spec/06 §6.2.3 left two readings open: Reading A (the wiki's
/// peek-bit / magnitude-VLC / sign-bit protocol) vs Reading B (a
/// single T02 codeword decoding the SIGNED component directly as
/// `position − 32`). The black-box P-frame conformance fixture pins
/// **Reading B** — the §6.10 item-1 structural evidence (code-length
/// symmetry around position 32, the 1-bit codeword at position 32 =
/// "no motion") was correct. Note Reading B still SUBSUMES the
/// wiki's peek-bit observation: T02's position-32 codeword is the
/// single bit `1`, so "if the next bit is 1 the component is 0" is
/// literally true of the signed VLC as well.
pub fn read_mv_component(br: &mut BitReader<'_>) -> Result<i32> {
    Ok(i32::from(crate::svq1_vlc::read_mv_component_position(br)?) - 32)
}

/// Read one differential motion vector: x component first, then y
/// (wiki §"Decoding Interframe Plane Data": "the x motion component
/// for this block is decoded from the stream and added to 0,
/// followed by the y component").
pub fn read_mv_differential(br: &mut BitReader<'_>) -> Result<(i32, i32)> {
    let dx = read_mv_component(br)?;
    let dy = read_mv_component(br)?;
    Ok((dx, dy))
}

/// Copy one 16×16 macroblock region from `reference` into `canvas`
/// (the SKIP path — wiki: "the block remains unchanged from the
/// previous I- or P- frame").
fn copy_mb_from_reference(
    canvas: &mut Svq1PlaneCanvas,
    reference: &Svq1PlaneCanvas,
    mb_x: usize,
    mb_y: usize,
) {
    for row in 0..MB_DIM {
        let src = (mb_y * MB_DIM + row) * reference.stride + mb_x * MB_DIM;
        let dst = (mb_y * MB_DIM + row) * canvas.stride + mb_x * MB_DIM;
        canvas.samples[dst..dst + MB_DIM].copy_from_slice(&reference.samples[src..src + MB_DIM]);
    }
}

/// Motion-compensate one 8×8 sub-block from `reference` into
/// `canvas` at integer-pel position `(base_col, base_row)` with the
/// half-pel MV `mv` (spec/06 §6.5).
fn mc_subblock_into(
    canvas: &mut Svq1PlaneCanvas,
    reference: &Svq1ReferencePlane<'_>,
    base_col: usize,
    base_row: usize,
    mv: Svq1Mv,
) {
    let patch = motion_compensate_block(reference, base_col as i32, base_row as i32, mv);
    for row in 0..MC_BLOCK_DIM {
        let dst = (base_row + row) * canvas.stride + base_col;
        canvas.samples[dst..dst + MC_BLOCK_DIM]
            .copy_from_slice(&patch[row * MC_BLOCK_DIM..(row + 1) * MC_BLOCK_DIM]);
    }
}

/// Clamp one MV component to the halfpel-domain window that keeps the
/// MC footprint of a `blk`-wide block at full-pel position `pos`
/// inside `[0, limit)` — see [`clamp_mv_to_reference_window`].
fn clamp_mv_component_to_window(v: i32, pos: usize, blk: usize, limit: usize) -> i32 {
    let lo = -2 * pos as i32;
    let hi = 2 * (limit as i32 - blk as i32 - pos as i32);
    v.clamp(lo, hi.max(lo))
}

/// Clamp a decoded motion vector to the **reference-window law**
/// pinned by the independently-minted INTER_4MV conformance fixture
/// (`docs/video/svq1/fixtures/inter-4mv/`, docs `f210f08`): before
/// motion compensation, each MV component is saturated (in half-pel
/// units) so that the block's entire MC footprint — including the
/// half-pel interpolation read at `+1` — stays inside the **padded
/// (macroblock-aligned) reference canvas** `[0, w) × [0, h)`.
///
/// This arbitrates the spec/06 §6.7 / §6.7.4 (#174) implementation-
/// defined edge question for real third-party streams, replacing edge
/// replication on the inter path:
///
/// * The clamp window is the PADDED canvas, not the visible frame:
///   the fixture's chroma planes (44 × 36 visible, 48 × 48 padded)
///   decode byte-exact only when MVs may reach into the §4.7.3
///   overhang region — which therefore must be decoded AND stored in
///   the reference (as [`decode_intra_plane`] /
///   [`decode_inter_plane`] already do). Clamping to the visible
///   window instead diverges on the chroma planes.
/// * The clamp applies to the MC read ONLY. The §6.8 MV cache stores
///   the UNCLAMPED vector (predictor + differential, §6.6-clipped) —
///   clamping the cached value too diverges on the very next
///   macroblock row of the fixture.
///
/// Because the clamped footprint is always in-window, the §6.7.2 edge
/// replication of [`Svq1ReferencePlane`] is unreachable from the
/// frame-decode path; it remains for direct [`crate::svq1_mc`] users.
///
/// `(x0, y0)` is the block's top-left full-pel position, `blk` its
/// edge length (16 for a whole INTER macroblock, 8 for an INTER_4MV
/// sub-block), `(w, h)` the padded reference canvas dimensions. For a
/// block that could never fit (overhang wider than the window) the
/// lower bound wins — unreachable for canvases padded to whole
/// macroblocks.
pub fn clamp_mv_to_reference_window(
    mv: Svq1Mv,
    x0: usize,
    y0: usize,
    blk: usize,
    w: usize,
    h: usize,
) -> Svq1Mv {
    Svq1Mv::new(
        clamp_mv_component_to_window(mv.x, x0, blk, w),
        clamp_mv_component_to_window(mv.y, y0, blk, h),
    )
}

/// Decode one INTERFRAME plane payload against `reference` (the
/// previous I- or P-frame's canvas for the same plane): every
/// macroblock in raster order reads its T03 coding mode, resolves
/// its motion (spec/06 — median predictor, `[-32, +31]` clip,
/// per-plane MV cache), motion-compensates the 16×16 baseline, and
/// (for INTER / INTER_4MV / INTRA) runs the breadth-first leaf walk
/// on top per wiki §"Decoding Interframe Plane Data" ("Once the
/// motion vector is fully decoded and the reference 16x16 block is
/// copied … repeat the same familiar intraframe decoding process").
pub fn decode_inter_plane(
    br: &mut BitReader<'_>,
    width: usize,
    height: usize,
    reference: &Svq1PlaneCanvas,
) -> Result<Svq1PlaneCanvas> {
    let mut canvas = Svq1PlaneCanvas::new(width, height);
    if reference.stride != canvas.stride || reference.rows != canvas.rows {
        return Err(Error::MissingReference);
    }
    let (mb_cols, mb_rows) = canvas.mb_grid();
    let mut cache = Svq1MvCache::new(mb_cols, mb_rows);
    let ref_plane = Svq1ReferencePlane::new(&reference.samples, reference.stride, reference.rows)
        .ok_or(Error::MissingReference)?;

    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let (block_row, block_col) = (mb_y * 2, mb_x * 2);
            match read_mb_mode(br)? {
                Svq1MbMode::Skip => {
                    copy_mb_from_reference(&mut canvas, reference, mb_x, mb_y);
                    cache.store_skip_intra(block_row, block_col);
                }
                Svq1MbMode::Intra => {
                    clear_mb(&mut canvas, mb_x, mb_y);
                    decode_mb_block_tree(br, &mut canvas, mb_x, mb_y, Svq1Half::Intra)?;
                    cache.store_skip_intra(block_row, block_col);
                }
                Svq1MbMode::Inter => {
                    let (dx, dy) = read_mv_differential(br)?;
                    // The cache stores the UNCLAMPED vector (it feeds
                    // later predictors); the MC read is clamped to the
                    // padded reference window — both sides pinned by
                    // the INTER_4MV conformance fixture (see
                    // `clamp_mv_to_reference_window`).
                    let mv = clamp_mv_to_reference_window(
                        cache.decode_inter(block_row, block_col, dx, dy),
                        mb_x * MB_DIM,
                        mb_y * MB_DIM,
                        MB_DIM,
                        canvas.stride,
                        canvas.rows,
                    );
                    for (sub_row, sub_col) in [(0usize, 0usize), (0, 1), (1, 0), (1, 1)] {
                        mc_subblock_into(
                            &mut canvas,
                            &ref_plane,
                            (block_col + sub_col) * MC_BLOCK_DIM,
                            (block_row + sub_row) * MC_BLOCK_DIM,
                            mv,
                        );
                    }
                    decode_mb_block_tree(br, &mut canvas, mb_x, mb_y, Svq1Half::Inter)?;
                }
                Svq1MbMode::Inter4mv => {
                    // The four differentials are positional on the
                    // wire; the predictor/store interleave is
                    // strictly serial inside `decode_inter_4mv`
                    // (spec/06 §6.4.5).
                    let mut diffs = [(0i32, 0i32); 4];
                    for d in &mut diffs {
                        *d = read_mv_differential(br)?;
                    }
                    let mvs = cache.decode_inter_4mv(block_row, block_col, diffs);
                    // SUBBLOCK_ORDER is top-left, top-right,
                    // bottom-left, bottom-right (§6.4.4). Each 8×8
                    // sub-block's MC read is clamped to the padded
                    // reference window INDEPENDENTLY (per-sub-block
                    // footprint); the cache stores kept the unclamped
                    // vectors above.
                    for (i, (sub_row, sub_col)) in [(0usize, 0usize), (0, 1), (1, 0), (1, 1)]
                        .into_iter()
                        .enumerate()
                    {
                        let x0 = (block_col + sub_col) * MC_BLOCK_DIM;
                        let y0 = (block_row + sub_row) * MC_BLOCK_DIM;
                        let mv = clamp_mv_to_reference_window(
                            mvs[i],
                            x0,
                            y0,
                            MC_BLOCK_DIM,
                            canvas.stride,
                            canvas.rows,
                        );
                        mc_subblock_into(&mut canvas, &ref_plane, x0, y0, mv);
                    }
                    decode_mb_block_tree(br, &mut canvas, mb_x, mb_y, Svq1Half::Inter)?;
                }
            }
        }
    }
    Ok(canvas)
}

/// Decode a complete SVQ1 frame — I, P, or B — against an optional
/// reference frame.
///
/// I-frames ignore `reference` and carry their own dimensions; P / B
/// frames REQUIRE `reference` (their headers carry no dimensions —
/// spec/01; the reference supplies both the geometry and the
/// prediction planes). B ("droppable") frames use the same forward
/// prediction as P frames per wiki §"Algorithm Basics" (SVQ1
/// B-frames are unidirectional) — the only difference is that a B
/// frame must never BECOME the reference, which is the caller's
/// frame-management concern (see the registry layer).
pub fn decode_frame(
    bytes: &[u8],
    reference: Option<&Svq1DecodedFrame>,
) -> Result<Svq1DecodedFrame> {
    let header = parse_frame_header(bytes)?;
    if header.picture_type == Svq1PictureType::Intra {
        return decode_intra_frame(bytes);
    }
    let reference = reference.ok_or(Error::MissingReference)?;
    let width = reference.width();
    let height = reference.height();

    let mut br = BitReader::new(bytes);
    for _ in 0..header.header_end_bit {
        br.read_bit()?;
    }

    let y = decode_inter_plane(&mut br, width, height, &reference.y)?;
    let u = decode_inter_plane(&mut br, chroma_dim(width), chroma_dim(height), &reference.u)?;
    let v = decode_inter_plane(&mut br, chroma_dim(width), chroma_dim(height), &reference.v)?;

    Ok(Svq1DecodedFrame { header, y, u, v })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-level writer for synthesising plane payloads.
    struct BitWriter {
        bytes: Vec<u8>,
        bit_pos: usize,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bit_pos: 0,
            }
        }

        fn push_bits(&mut self, width: u32, value: u32) {
            for i in (0..width).rev() {
                let bit = ((value >> i) & 1) as u8;
                if self.bit_pos / 8 >= self.bytes.len() {
                    self.bytes.push(0);
                }
                self.bytes[self.bit_pos / 8] |= bit << (7 - (self.bit_pos % 8));
                self.bit_pos += 1;
            }
        }

        /// Append one VLC codeword from a `(codeword, length)` table
        /// at the given alphabet position.
        fn push_code(&mut self, table: &[(u16, u8)], position: usize) {
            let (cw, len) = table[position];
            self.push_bits(u32::from(len), u32::from(cw));
        }
    }

    use crate::svq1_codebook::SVQ1_VLC_INTRA_MEAN;
    use crate::svq1_codebook::{codebook_vector_in_half, half_byte_offset_in_payload};
    use crate::svq1_vlc::{intra_mean_decoder, intra_stage_count_table};

    /// Emit a full intra macroblock that subdivides straight to four
    /// L=3 leaves, all mean-only with the given mean.
    fn push_flat_mb(w: &mut BitWriter, mean: u8) {
        w.push_bits(1, 1); // L=5 subdivide
        w.push_bits(1, 1); // L=4 top subdivide
        w.push_bits(1, 1); // L=4 bottom subdivide
        for _ in 0..4 {
            w.push_bits(1, 0); // L=3 leaf
                               // stage count 0 (mean-only) = alphabet position 1
            w.push_code(&intra_stage_count_table(Svq1Level::L3).0, 1);
            w.push_code(&SVQ1_VLC_INTRA_MEAN.0, usize::from(mean));
        }
    }

    /// A 16×16 single-MB plane of four mean-only L=3 leaves decodes
    /// to a uniform plane at the mean value.
    #[test]
    fn flat_mean_only_macroblock() {
        let mut w = BitWriter::new();
        push_flat_mb(&mut w, 61);
        let mut br = BitReader::new(&w.bytes);
        let canvas = decode_intra_plane(&mut br, 16, 16).expect("plane decodes");
        assert!(canvas.samples.iter().all(|&s| s == 61));
    }

    /// Distinct means per L=3 quadrant land at the §3.4 geometry:
    /// leaves arrive top-left, top-right, bottom-left, bottom-right
    /// (L=5 splits top/bottom, each L=4 splits left/right, and the
    /// breadth-first order visits top-MB's children before
    /// bottom-MB's).
    #[test]
    fn quadrant_means_follow_subdivision_geometry() {
        let mut w = BitWriter::new();
        w.push_bits(1, 1); // L=5 subdivide → top, bottom L=4
        w.push_bits(1, 1); // top L=4 subdivide → left, right L=3
        w.push_bits(1, 1); // bottom L=4 subdivide → left, right L=3
        for mean in [10u8, 20, 30, 40] {
            w.push_bits(1, 0); // L=3 leaf
            w.push_code(&intra_stage_count_table(Svq1Level::L3).0, 1);
            w.push_code(&SVQ1_VLC_INTRA_MEAN.0, usize::from(mean));
        }
        let mut br = BitReader::new(&w.bytes);
        let canvas = decode_intra_plane(&mut br, 16, 16).expect("plane decodes");
        // Quadrant sample probes: (x, y) → expected mean.
        let probe = |x: usize, y: usize| canvas.samples[y * canvas.stride + x];
        assert_eq!(probe(0, 0), 10, "top-left");
        assert_eq!(probe(15, 0), 20, "top-right");
        assert_eq!(probe(0, 15), 30, "bottom-left");
        assert_eq!(probe(15, 15), 40, "bottom-right");
    }

    /// The spec/04 §4.8 worked example embedded in a real bit
    /// stream: an L=1 (4×4) branch subdividing to two L=0 leaves,
    /// the first carrying `N = 2, mean = 61, stage indices 4 and
    /// 14`. The §4.8.3 output block must appear in the canvas IF
    /// the intra L=0 codebook page (Hypothesis A) holds the §4.8.2
    /// vectors at (stage 1, vec 4) / (stage 2, vec 14). This test
    /// self-conditions: it asserts the pipeline reproduces
    /// `reconstruct_leaf`'s output for whatever the staged bytes
    /// are, so it stays valid under either §14.8 hypothesis while
    /// the conformance fixture (tests/) pins which one is real.
    #[test]
    fn two_stage_leaf_matches_reconstruct_leaf() {
        let mut w = BitWriter::new();
        w.push_bits(1, 1); // L=5 subdivide
        w.push_bits(1, 1); // L=4 top
        w.push_bits(1, 1); // L=4 bottom
                           // Top-left L=3: subdivide all the way to a pair of L=0
                           // leaves on the first L=1 block.
        w.push_bits(1, 1); // L=3 tl subdivide → two L=2
                           // Remaining three L=3 leaves: mean-only 0.
        for _ in 0..3 {
            w.push_bits(1, 0);
            w.push_code(&intra_stage_count_table(Svq1Level::L3).0, 1);
            w.push_code(&SVQ1_VLC_INTRA_MEAN.0, 0);
        }
        // Two L=2 blocks (8×4): first subdivides, second mean-only.
        w.push_bits(1, 1); // L=2 first → two L=1
        w.push_bits(1, 0); // L=2 second leaf
        w.push_code(&intra_stage_count_table(Svq1Level::L2).0, 1);
        w.push_code(&SVQ1_VLC_INTRA_MEAN.0, 0);
        // Two L=1 blocks (4×4): first subdivides to two L=0, second
        // mean-only.
        w.push_bits(1, 1);
        w.push_bits(1, 0);
        w.push_code(&intra_stage_count_table(Svq1Level::L1).0, 1);
        w.push_code(&SVQ1_VLC_INTRA_MEAN.0, 0);
        // Two L=0 leaves (4×2, no subdivide bit): the §4.8 worked
        // example (N=2, mean 61, indices 4 and 14), then mean-only 0.
        w.push_code(&intra_stage_count_table(Svq1Level::L0).0, 3); // N=2
        w.push_code(&SVQ1_VLC_INTRA_MEAN.0, 61);
        w.push_bits(4, 4); // stage 1 index
        w.push_bits(4, 14); // stage 2 index
        w.push_code(&intra_stage_count_table(Svq1Level::L0).0, 1); // N=0
        w.push_code(&SVQ1_VLC_INTRA_MEAN.0, 0);

        let mut br = BitReader::new(&w.bytes);
        let canvas = decode_intra_plane(&mut br, 16, 16).expect("plane decodes");

        // Expected: reconstruct_leaf on the staged intra L=0 page.
        let page = codebook_half(Svq1Level::L0, Svq1Half::Intra).unwrap();
        let expected = reconstruct_leaf(
            Svq1Level::L0,
            page,
            &[0u8; 8],
            61,
            &[
                LeafStage {
                    stage: 1,
                    vec_idx: 4,
                },
                LeafStage {
                    stage: 2,
                    vec_idx: 14,
                },
            ],
        )
        .unwrap();
        let mut got = Vec::new();
        for row in 0..2 {
            let start = row * canvas.stride;
            got.extend_from_slice(&canvas.samples[start..start + 4]);
        }
        assert_eq!(got, expected);
        // And the second L=0 leaf (rows 2..4 of cols 0..4) is zero.
        assert_eq!(canvas.samples[2 * canvas.stride], 0);
    }

    /// Intra SKIP (stage-count position 0) is rejected with
    /// `UnexpectedIntraSkip`.
    #[test]
    fn intra_skip_rejected() {
        let mut w = BitWriter::new();
        w.push_bits(1, 1);
        w.push_bits(1, 1);
        w.push_bits(1, 1);
        w.push_bits(1, 0); // L=3 leaf
        w.push_code(&intra_stage_count_table(Svq1Level::L3).0, 0); // SKIP
        let mut br = BitReader::new(&w.bytes);
        assert!(matches!(
            decode_intra_plane(&mut br, 16, 16),
            Err(Error::UnexpectedIntraSkip)
        ));
    }

    /// A truncated plane payload surfaces `Truncated`. The payload
    /// ends exactly at a byte boundary mid-leaf (after the L=5/L=4
    /// subdivide bits, one L=3 leaf bit, and its 1-bit stage-count
    /// codeword, the intra mean VLC — shortest codeword 4 bits —
    /// runs off the end of the 8-bit buffer).
    #[test]
    fn truncated_plane_payload() {
        let mut w = BitWriter::new();
        w.push_bits(1, 1); // L=5 subdivide
        w.push_bits(1, 1); // L=4 top subdivide
        w.push_bits(1, 1); // L=4 bottom subdivide
        w.push_bits(1, 0); // L=3 leaf
        w.push_code(&intra_stage_count_table(Svq1Level::L3).0, 1); // N=0
        w.push_bits(3, 0); // 3 zero bits, then end-of-stream mid-mean
        assert_eq!(w.bytes.len(), 1);
        let mut br = BitReader::new(&w.bytes);
        assert!(matches!(
            decode_intra_plane(&mut br, 16, 16),
            Err(Error::Truncated)
        ));
    }

    /// Pinned page offsets: the canonical 23 040-byte region tiles
    /// level-major DESCENDING (L=3 → L=0), intra half before inter
    /// half within each level (empirically pinned by the black-box
    /// conformance fixture; see `svq1_codebook::half_byte_offset_in_payload`).
    #[test]
    fn pinned_page_offsets() {
        for (level, intra, inter) in [
            (Svq1Level::L3, 0usize, 6144usize),
            (Svq1Level::L2, 12288, 15360),
            (Svq1Level::L1, 18432, 19968),
            (Svq1Level::L0, 21504, 22272),
        ] {
            assert_eq!(
                half_byte_offset_in_payload(level, Svq1Half::Intra),
                Some(intra),
                "{level:?} intra"
            );
            assert_eq!(
                half_byte_offset_in_payload(level, Svq1Half::Inter),
                Some(inter),
                "{level:?} inter"
            );
        }
        assert_eq!(
            half_byte_offset_in_payload(Svq1Level::L4, Svq1Half::Intra),
            None
        );
        // Every page is addressable at its full canonical size (the
        // canonical region carries the block-shape-LUT dual-use
        // front bytes AND the codebook-tail.csv window, so no page
        // is truncated).
        for level in [Svq1Level::L0, Svq1Level::L1, Svq1Level::L2, Svq1Level::L3] {
            for half in [Svq1Half::Intra, Svq1Half::Inter] {
                let page = codebook_half(level, half).unwrap();
                assert_eq!(
                    page.len(),
                    level.codebook_bytes_per_half().unwrap(),
                    "{level:?} {half:?}"
                );
            }
        }
        // The final page (inter L=0) ends exactly at the canonical
        // boundary; its last vector resolves.
        let page = codebook_half(Svq1Level::L0, Svq1Half::Inter).unwrap();
        assert!(codebook_vector_in_half(page, Svq1Level::L0, 6, 15).is_some());
        // The wiki §4.8 worked-example vectors live in this page at
        // (stage 1, vec 4) / (stage 2, vec 14).
        assert_eq!(
            codebook_vector_in_half(page, Svq1Level::L0, 1, 4).unwrap(),
            &[7i8, -16, -10, 20, 7, -17, -10, 20]
        );
        assert_eq!(
            codebook_vector_in_half(page, Svq1Level::L0, 2, 14).unwrap(),
            &[-13i8, -6, -1, -4, 25, 37, -2, -35]
        );
    }

    /// A minimal full intra frame (header + Y + U + V) through
    /// `decode_intra_frame`: 16×16 luma (one MB), 4×4 chroma planes
    /// (one padded MB each — the canvas pads them to 16×16 and the
    /// visible crop returns 4×4).
    #[test]
    fn minimal_intra_frame_decodes() {
        let mut w = BitWriter::new();
        // Header: frame_code 0x40, tref 0, picture type I, unknown
        // 2+2+1 bits, frame size code 7 (explicit 16×16), checksum
        // absent, flag_1 absent. Mirrors header.rs tests.
        w.push_bits(22, 0x40);
        w.push_bits(8, 0);
        w.push_bits(2, 0);
        w.push_bits(2, 0);
        w.push_bits(2, 0);
        w.push_bits(1, 0);
        w.push_bits(3, 7);
        w.push_bits(12, 16);
        w.push_bits(12, 16);
        w.push_bits(1, 0);
        w.push_bits(1, 0);
        // Y plane: one MB, flat 61.
        push_flat_mb(&mut w, 61);
        // U plane: one padded MB (4×4 visible), flat 90.
        push_flat_mb(&mut w, 90);
        // V plane: flat 200.
        push_flat_mb(&mut w, 200);

        let frame = decode_intra_frame(&w.bytes).expect("frame decodes");
        assert_eq!(frame.width(), 16);
        assert_eq!(frame.height(), 16);
        assert!(frame.y.visible().iter().all(|&s| s == 61));
        assert_eq!(frame.u.width, 4);
        assert_eq!(frame.u.visible(), vec![90u8; 16]);
        assert_eq!(frame.v.visible(), vec![200u8; 16]);
        // Doc-anchor: the intra mean decoder is the single T01 table.
        assert_eq!(intra_mean_decoder().min_value(), 0);
    }

    #[test]
    fn reference_window_clamp_is_inert_for_in_window_footprints() {
        // A 16×16 block at (16, 16) in a 176×144 canvas: MVs whose
        // halfpel footprint stays inside are untouched.
        for mv in [
            Svq1Mv::new(0, 0),
            Svq1Mv::new(-32, -32),
            Svq1Mv::new(31, 31),
            Svq1Mv::new(-7, 13),
        ] {
            assert_eq!(
                clamp_mv_to_reference_window(mv, 16, 16, 16, 176, 144),
                mv,
                "in-window MV must pass through"
            );
        }
    }

    #[test]
    fn reference_window_clamp_saturates_right_edge_including_halfpel_read() {
        // The INTER_4MV-fixture case: MB column 10 of a 176-wide
        // plane (x0 = 160). mv.x = +4 (full-pel +2) reads x = 177;
        // mv.x = +1 (half-pel) reads x = 176; both exceed w−1 = 175
        // and clamp to 0. The y component is untouched.
        for bad_x in [4, 1, 2, 31] {
            assert_eq!(
                clamp_mv_to_reference_window(Svq1Mv::new(bad_x, -8), 160, 64, 16, 176, 144),
                Svq1Mv::new(0, -8),
            );
        }
        // Negative x is unconstrained on the right edge (until the
        // left bound at −2·x0 = −320, far past the §6.6 clip).
        assert_eq!(
            clamp_mv_to_reference_window(Svq1Mv::new(-9, -8), 160, 64, 16, 176, 144),
            Svq1Mv::new(-9, -8),
        );
    }

    #[test]
    fn reference_window_clamp_saturates_all_four_edges() {
        // Top-left block: negative components clamp to 0.
        assert_eq!(
            clamp_mv_to_reference_window(Svq1Mv::new(-5, -1), 0, 0, 16, 176, 144),
            Svq1Mv::new(0, 0),
        );
        // Bottom edge (y0 = 128 of 144): positive y clamps to 0.
        assert_eq!(
            clamp_mv_to_reference_window(Svq1Mv::new(0, 3), 0, 128, 16, 176, 144),
            Svq1Mv::new(0, 0),
        );
        // One row up (y0 = 112): up to +2·16 = 32 would fit; the §6.6
        // range keeps inputs ≤ 31, all of which pass through.
        assert_eq!(
            clamp_mv_to_reference_window(Svq1Mv::new(0, 31), 0, 112, 16, 176, 144),
            Svq1Mv::new(0, 31),
        );
    }

    #[test]
    fn reference_window_clamp_uses_per_subblock_footprint() {
        // An 8×8 INTER_4MV sub-block at x0 = 168 (the right half of
        // MB column 10): +4 clamps to 0, while the LEFT sub-block of
        // the same MB (x0 = 160) may keep +4 with blk = 8 (footprint
        // 160..170 ≤ 175).
        assert_eq!(
            clamp_mv_to_reference_window(Svq1Mv::new(4, 0), 168, 64, 8, 176, 144),
            Svq1Mv::new(0, 0),
        );
        assert_eq!(
            clamp_mv_to_reference_window(Svq1Mv::new(4, 0), 160, 64, 8, 176, 144),
            Svq1Mv::new(4, 0),
        );
    }
}
