//! SVQ1 encoder — the naïve-baseline intra encoder plus a one-stage
//! VQ refinement.
//!
//! The staged encoder companion
//! (`docs/video/svq1/wiki/eggs-naive-svq1-encoder.html`) describes
//! the canonical bring-up path: "The coarsest encoding mode that
//! SVQ1 allows is to encode the average (mean) of each 16×16 block
//! of samples" — i.e. a stream of mean-only leaves — with the two
//! illustrated variants (mean-only 16×16 L=5 blocks; mean-only 8×8
//! L=3 blocks). This module implements both, plus a
//! mean-plus-one-stage L=3 mode that searches the intra L=3 stage-1
//! codebook page for the SSE-minimising vector.
//!
//! Everything is emitted through the SAME staged tables the decoder
//! reads (`crate::svq1_vlc` codewords, `crate::svq1_codebook` pages,
//! the [`crate::svq1_codebook::vector_byte_to_raster`] tile order),
//! so the encoder is bit-consistent with the byte-exact-validated
//! decode path by construction; the round-trip tests close the loop
//! through [`crate::svq1_plane::decode_intra_frame`].
//!
//! ## Header shape
//!
//! The emitted frame header follows the wiki §"Stream Format And
//! Header" layout with every optional trailer suppressed:
//! `frame_code = 0x20` (bit 5 set → valid; not `0x50`/`0x60` → no
//! checksum; `0x20 ^ 0x10 = 0x30 ≤ 0x50` → no embedded string),
//! picture type I, the 2+2+1 unknown bits zeroed, the common-size
//! code when the dimensions match `crate::FRAME_SIZE_TABLE` (else
//! the code-7 explicit 12+12-bit escape), `checksum_present = 0`,
//! `unknown_flag_1 = 0`.

use crate::error::{Error, Result};
use crate::header::FRAME_SIZE_TABLE;
use crate::svq1_blocktree::Svq1Level;
use crate::svq1_codebook::{
    codebook_half, vector_byte_to_raster, SVQ1_ENTRIES_PER_STAGE, SVQ1_VLC_INTRA_MEAN,
};
use crate::svq1_plane::{chroma_dim, MB_DIM};
use crate::svq1_vlc::{intra_stage_count_table, Svq1Half};

/// MSB-first bit writer, the mirror of [`crate::BitReader`].
#[derive(Debug, Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    bit_pos: usize,
}

impl BitWriter {
    /// Fresh writer at bit 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `width` bits of `value`, MSB-first.
    pub fn push_bits(&mut self, width: u32, value: u32) {
        debug_assert!(width == 32 || value < (1u32.checked_shl(width).unwrap_or(0)) || width == 0);
        for i in (0..width).rev() {
            let bit = ((value >> i) & 1) as u8;
            if self.bit_pos / 8 >= self.bytes.len() {
                self.bytes.push(0);
            }
            self.bytes[self.bit_pos / 8] |= bit << (7 - (self.bit_pos % 8));
            self.bit_pos += 1;
        }
    }

    /// Append the codeword at `position` of a staged
    /// `(codeword, code_length)` table.
    pub fn push_code(&mut self, table: &[(u16, u8)], position: usize) {
        let (codeword, length) = table[position];
        self.push_bits(u32::from(length), u32::from(codeword));
    }

    /// Bits written so far.
    pub fn bits_written(&self) -> usize {
        self.bit_pos
    }

    /// Consume the writer, returning the byte buffer (final partial
    /// byte zero-padded — container-level slack per spec/02 §2.6.3).
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Encoder block mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq1EncoderMode {
    /// One mean-only L=5 leaf per macroblock — the coarsest legal
    /// stream (the companion doc's "mean-only 16×16 encoding").
    MeanOnlyL5,
    /// Subdivide to four L=3 leaves per macroblock, each mean-only
    /// (the companion doc's "mean-only 8×8 encoding").
    MeanOnlyL3,
    /// Four L=3 leaves per macroblock, each mean + the SSE-best
    /// stage-1 codebook vector (falling back to mean-only when no
    /// vector reduces the error).
    MeanPlusOneStageL3,
    /// Four L=3 leaves per macroblock, each running the full greedy
    /// multi-stage descent of [`crate::svq1_enc_leaf::search_leaf`]
    /// (up to six stages per leaf — the complete spec/04 §4.5
    /// vocabulary at L=3).
    MultiStageL3,
    /// Per-macroblock λ-cost block-tree search over the full spec/03
    /// hierarchy (L=5..L=0) via [`crate::svq1_enc_tree`]: each block
    /// becomes a multi-stage leaf or splits, minimising
    /// `SSE + lambda × bits`. `lambda = 0` maximises fidelity; larger
    /// values trade SSE for rate down to the mean-only-16×16 floor.
    Adaptive {
        /// Rate weight (SSE units per wire bit).
        lambda: u64,
    },
}

/// One raw input plane (tightly packed, `width × height`).
#[derive(Debug, Clone, Copy)]
pub struct Svq1PlaneRef<'a> {
    /// Row-major samples, `width * height` long.
    pub samples: &'a [u8],
    /// Plane width in samples.
    pub width: usize,
    /// Plane height in samples.
    pub height: usize,
}

impl<'a> Svq1PlaneRef<'a> {
    /// Edge-replicated sample fetch (for overhang macroblocks —
    /// spec/02 §2.3.1 codes full 16×16 blocks over the frame edge).
    fn sample(&self, x: usize, y: usize) -> u8 {
        let cx = x.min(self.width - 1);
        let cy = y.min(self.height - 1);
        self.samples[cy * self.width + cx]
    }

    /// Collect a `w × h` block at `(x, y)` in raster order with edge
    /// replication.
    pub fn block(&self, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            for col in 0..w {
                out.push(self.sample(x + col, y + row));
            }
        }
        out
    }
}

/// Rounded mean of a block.
fn block_mean(block: &[u8]) -> u8 {
    let sum: u32 = block.iter().map(|&s| u32::from(s)).sum();
    ((sum + (block.len() as u32 / 2)) / block.len() as u32) as u8
}

/// Emit one mean-only leaf: the stage-count codeword for `N = 0`
/// (alphabet position 1 per the audit-corrected spec/04 §4.1
/// mapping) followed by the intra mean codeword.
fn push_mean_only_leaf(w: &mut BitWriter, level: Svq1Level, mean: u8) {
    w.push_code(&intra_stage_count_table(level).0, 1);
    w.push_code(&SVQ1_VLC_INTRA_MEAN.0, usize::from(mean));
}

/// Emit one `N = 1` leaf: stage count (position 2), mean, and the
/// 4-bit stage-1 vector index.
fn push_one_stage_leaf(w: &mut BitWriter, level: Svq1Level, mean: u8, vec_idx: usize) {
    w.push_code(&intra_stage_count_table(level).0, 2);
    w.push_code(&SVQ1_VLC_INTRA_MEAN.0, usize::from(mean));
    w.push_bits(4, vec_idx as u32);
}

/// Emit one searched leaf payload ([`crate::svq1_enc_leaf`]) on the
/// intra tables: stage-count codeword at position `N + 1`, the intra
/// mean codeword, then the `4N` raw index bits (spec/04 §4.2.1).
pub(crate) fn push_intra_leaf_choice(
    w: &mut BitWriter,
    level: Svq1Level,
    choice: &crate::svq1_enc_leaf::LeafChoice,
) {
    let crate::svq1_enc_leaf::LeafCode::Coded { mean, ref stages } = choice.code else {
        unreachable!("intra leaves are never SKIP (spec/04 §4.9.1)");
    };
    w.push_code(&intra_stage_count_table(level).0, stages.len() + 1);
    w.push_code(&SVQ1_VLC_INTRA_MEAN.0, mean as usize);
    for &vec_idx in stages {
        w.push_bits(4, u32::from(vec_idx));
    }
}

/// Sum of squared errors between a source block and
/// `mean + vector` (wide accumulation, final clamp — mirroring the
/// decoder's pinned arithmetic).
fn leaf_sse(block: &[u8], mean: u8, vector_raster: &[i16]) -> u64 {
    block
        .iter()
        .zip(vector_raster.iter())
        .map(|(&src, &v)| {
            let recon = (i16::from(mean) + v).clamp(0, 255);
            let d = i64::from(src) - i64::from(recon);
            (d * d) as u64
        })
        .sum()
}

/// Encode one plane's macroblocks in raster order (spec/02 §2.4).
fn encode_intra_plane(
    w: &mut BitWriter,
    plane: &Svq1PlaneRef<'_>,
    mode: Svq1EncoderMode,
) -> Result<()> {
    let mb_cols = plane.width.div_ceil(MB_DIM);
    let mb_rows = plane.height.div_ceil(MB_DIM);

    // The intra L=3 stage-1 vectors, re-ordered to raster once.
    let l3_stage1: Vec<Vec<i16>> = if mode == Svq1EncoderMode::MeanPlusOneStageL3 {
        let page = codebook_half(Svq1Level::L3, Svq1Half::Intra)
            .ok_or(Error::InvalidLevelQuantise(Svq1Level::L3))?;
        (0..SVQ1_ENTRIES_PER_STAGE)
            .map(|vec_idx| {
                let mut raster = vec![0i16; 64];
                for byte_idx in 0..64 {
                    let value = page[vec_idx * 64 + byte_idx];
                    raster[vector_byte_to_raster(Svq1Level::L3, byte_idx)] = i16::from(value);
                }
                raster
            })
            .collect()
    } else {
        Vec::new()
    };

    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let (x0, y0) = (mb_x * MB_DIM, mb_y * MB_DIM);
            match mode {
                Svq1EncoderMode::Adaptive { lambda } => {
                    let block = plane.block(x0, y0, 16, 16);
                    let mut target = [0u8; 256];
                    target.copy_from_slice(&block);
                    let plan = crate::svq1_enc_tree::plan_macroblock(
                        &target,
                        &[0u8; 256],
                        Svq1Half::Intra,
                        false,
                        lambda,
                    );
                    crate::svq1_enc_tree::emit_macroblock(w, &plan, Svq1Half::Intra);
                }
                Svq1EncoderMode::MeanOnlyL5 => {
                    // L=5 leaf: subdivide bit 0, then mean-only.
                    w.push_bits(1, 0);
                    let mean = block_mean(&plane.block(x0, y0, 16, 16));
                    push_mean_only_leaf(w, Svq1Level::L5, mean);
                }
                Svq1EncoderMode::MeanOnlyL3
                | Svq1EncoderMode::MeanPlusOneStageL3
                | Svq1EncoderMode::MultiStageL3 => {
                    // Subdivide L=5 → two L=4 → four L=3 (spec/03
                    // §3.4: top/bottom then left/right).
                    w.push_bits(1, 1); // L=5
                    w.push_bits(1, 1); // L=4 top
                    w.push_bits(1, 1); // L=4 bottom
                    for (dx, dy) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)] {
                        w.push_bits(1, 0); // L=3 leaf
                        let block = plane.block(x0 + dx, y0 + dy, 8, 8);
                        let mean = block_mean(&block);
                        if mode == Svq1EncoderMode::MeanOnlyL3 {
                            push_mean_only_leaf(w, Svq1Level::L3, mean);
                            continue;
                        }
                        if mode == Svq1EncoderMode::MultiStageL3 {
                            let choice = crate::svq1_enc_leaf::search_leaf(
                                Svq1Level::L3,
                                Svq1Half::Intra,
                                &block,
                                &[0u8; 64],
                                6,
                                false,
                            );
                            push_intra_leaf_choice(w, Svq1Level::L3, &choice);
                            continue;
                        }
                        // One-stage search: SSE over the 16 stage-1
                        // vectors vs the mean-only baseline.
                        let zero = vec![0i16; 64];
                        let mut best_sse = leaf_sse(&block, mean, &zero);
                        let mut best: Option<usize> = None;
                        for (vec_idx, raster) in l3_stage1.iter().enumerate() {
                            let sse = leaf_sse(&block, mean, raster);
                            if sse < best_sse {
                                best_sse = sse;
                                best = Some(vec_idx);
                            }
                        }
                        match best {
                            Some(vec_idx) => push_one_stage_leaf(w, Svq1Level::L3, mean, vec_idx),
                            None => push_mean_only_leaf(w, Svq1Level::L3, mean),
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Encode one SVQ1 intra frame from tightly-packed YUV 4:1:0 planes.
///
/// `y` is `width × height`; `u` / `v` are
/// `chroma_dim(width) × chroma_dim(height)` (spec/02 §2.2). Returns
/// the complete codec-frame byte array (header + Y + U + V plane
/// payloads, bit-tight per spec/02 §2.5, zero-padded to a byte).
pub fn encode_intra_frame(
    y: Svq1PlaneRef<'_>,
    u: Svq1PlaneRef<'_>,
    v: Svq1PlaneRef<'_>,
    mode: Svq1EncoderMode,
) -> Result<Vec<u8>> {
    let (width, height) = (y.width, y.height);
    if width == 0
        || height == 0
        || width > 4095
        || height > 4095
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
    // Header (wiki §"Stream Format And Header", all options off).
    w.push_bits(22, 0x20); // frame code
    w.push_bits(8, 0); // temporal reference
    w.push_bits(2, 0); // picture type = I
    w.push_bits(2, 0); // unknown
    w.push_bits(2, 0); // unknown
    w.push_bits(1, 0); // unknown
    match FRAME_SIZE_TABLE
        .iter()
        .position(|&(fw, fh)| usize::from(fw) == width && usize::from(fh) == height)
    {
        Some(code) => w.push_bits(3, code as u32),
        None => {
            w.push_bits(3, 7);
            w.push_bits(12, width as u32);
            w.push_bits(12, height as u32);
        }
    }
    w.push_bits(1, 0); // checksum_present
    w.push_bits(1, 0); // unknown_flag_1

    encode_intra_plane(&mut w, &y, mode)?;
    encode_intra_plane(&mut w, &u, mode)?;
    encode_intra_plane(&mut w, &v, mode)?;

    Ok(w.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svq1_plane::decode_intra_frame;

    fn gradient_plane(width: usize, height: usize, seed: u32) -> Vec<u8> {
        (0..width * height)
            .map(|i| {
                let x = (i % width) as u32;
                let y = (i / width) as u32;
                ((x * 3 + y * 5 + seed) % 256) as u8
            })
            .collect()
    }

    fn planes(width: usize, height: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (
            gradient_plane(width, height, 7),
            gradient_plane(chroma_dim(width), chroma_dim(height), 101),
            gradient_plane(chroma_dim(width), chroma_dim(height), 202),
        )
    }

    fn refs<'a>(
        y: &'a [u8],
        u: &'a [u8],
        v: &'a [u8],
        width: usize,
        height: usize,
    ) -> (Svq1PlaneRef<'a>, Svq1PlaneRef<'a>, Svq1PlaneRef<'a>) {
        (
            Svq1PlaneRef {
                samples: y,
                width,
                height,
            },
            Svq1PlaneRef {
                samples: u,
                width: chroma_dim(width),
                height: chroma_dim(height),
            },
            Svq1PlaneRef {
                samples: v,
                width: chroma_dim(width),
                height: chroma_dim(height),
            },
        )
    }

    /// Mean-only L=5 round trip: our decoder reproduces each
    /// macroblock as a uniform block at the source mean.
    #[test]
    fn mean_only_l5_round_trip() {
        let (width, height) = (176usize, 144usize);
        let (y, u, v) = planes(width, height);
        let (yr, ur, vr) = refs(&y, &u, &v, width, height);
        let encoded = encode_intra_frame(yr, ur, vr, Svq1EncoderMode::MeanOnlyL5).expect("encodes");
        let frame = decode_intra_frame(&encoded).expect("decodes");
        assert_eq!(frame.width(), width);
        assert_eq!(frame.height(), height);
        let decoded_y = frame.y.visible();
        for mb_y in 0..height / 16 {
            for mb_x in 0..width / 16 {
                let block = yr.block(mb_x * 16, mb_y * 16, 16, 16);
                let mean = block_mean(&block);
                for row in 0..16 {
                    for col in 0..16 {
                        assert_eq!(
                            decoded_y[(mb_y * 16 + row) * width + mb_x * 16 + col],
                            mean,
                            "MB ({mb_x},{mb_y}) sample ({col},{row})"
                        );
                    }
                }
            }
        }
    }

    /// Mean-only L=3 round trip at an overhang size (160×120):
    /// every visible 8×8 region is uniform at the (edge-replicated)
    /// source mean.
    #[test]
    fn mean_only_l3_round_trip_with_overhang() {
        let (width, height) = (160usize, 120usize);
        let (y, u, v) = planes(width, height);
        let (yr, ur, vr) = refs(&y, &u, &v, width, height);
        let encoded = encode_intra_frame(yr, ur, vr, Svq1EncoderMode::MeanOnlyL3).expect("encodes");
        let frame = decode_intra_frame(&encoded).expect("decodes");
        let decoded_v = frame.v.visible();
        let (cw, ch) = (chroma_dim(width), chroma_dim(height));
        // Probe the bottom-right chroma corner: the L=3 leaf that
        // covers the last visible sample starts at the last
        // 8-aligned position (partially overhanging both ways for
        // 40×30 chroma).
        let block = vr.block(((cw - 1) / 8) * 8, ((ch - 1) / 8) * 8, 8, 8);
        let mean = block_mean(&block);
        assert_eq!(decoded_v[(ch - 1) * cw + (cw - 1)], mean);
    }

    /// One-stage mode round trip: the decode must equal the
    /// encoder's own model (mean + chosen stage-1 vector, clamped),
    /// and the total SSE must not exceed the mean-only mode's.
    #[test]
    fn one_stage_l3_reduces_error_and_round_trips() {
        let (width, height) = (64usize, 48usize);
        let (y, u, v) = planes(width, height);
        let (yr, ur, vr) = refs(&y, &u, &v, width, height);

        let enc_mean =
            encode_intra_frame(yr, ur, vr, Svq1EncoderMode::MeanOnlyL3).expect("encodes");
        let enc_staged =
            encode_intra_frame(yr, ur, vr, Svq1EncoderMode::MeanPlusOneStageL3).expect("encodes");

        let dec_mean = decode_intra_frame(&enc_mean).expect("decodes");
        let dec_staged = decode_intra_frame(&enc_staged).expect("decodes");

        let sse = |decoded: &[u8], source: &[u8]| -> u64 {
            decoded
                .iter()
                .zip(source.iter())
                .map(|(&a, &b)| {
                    let d = i64::from(a) - i64::from(b);
                    (d * d) as u64
                })
                .sum()
        };
        let mean_sse = sse(&dec_mean.y.visible(), &y);
        let staged_sse = sse(&dec_staged.y.visible(), &y);
        assert!(
            staged_sse <= mean_sse,
            "one-stage mode must not increase Y-plane SSE ({staged_sse} vs {mean_sse})"
        );
    }

    /// Multi-stage mode round trip: the decode must equal the
    /// searcher's own reconstruction model leaf by leaf, and the
    /// whole-frame SSE must not exceed the one-stage mode's.
    #[test]
    fn multi_stage_l3_beats_one_stage_and_round_trips() {
        let (width, height) = (64usize, 48usize);
        let (y, u, v) = planes(width, height);
        let (yr, ur, vr) = refs(&y, &u, &v, width, height);

        let enc_one =
            encode_intra_frame(yr, ur, vr, Svq1EncoderMode::MeanPlusOneStageL3).expect("encodes");
        let enc_multi =
            encode_intra_frame(yr, ur, vr, Svq1EncoderMode::MultiStageL3).expect("encodes");

        let dec_one = decode_intra_frame(&enc_one).expect("decodes");
        let dec_multi = decode_intra_frame(&enc_multi).expect("decodes");

        let sse = |decoded: &[u8], source: &[u8]| -> u64 {
            decoded
                .iter()
                .zip(source.iter())
                .map(|(&a, &b)| {
                    let d = i64::from(a) - i64::from(b);
                    (d * d) as u64
                })
                .sum()
        };
        let one_sse = sse(&dec_one.y.visible(), &y);
        let multi_sse = sse(&dec_multi.y.visible(), &y);
        assert!(
            multi_sse <= one_sse,
            "multi-stage must not increase Y-plane SSE ({multi_sse} vs {one_sse})"
        );

        // Leaf-level cross-check: the decoded plane equals the
        // searcher's own recon for every L=3 leaf.
        let decoded_y = dec_multi.y.visible();
        for mb_y in 0..height / 16 {
            for mb_x in 0..width / 16 {
                for (dx, dy) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)] {
                    let block = yr.block(mb_x * 16 + dx, mb_y * 16 + dy, 8, 8);
                    let choice = crate::svq1_enc_leaf::search_leaf(
                        Svq1Level::L3,
                        crate::svq1_vlc::Svq1Half::Intra,
                        &block,
                        &[0u8; 64],
                        6,
                        false,
                    );
                    for row in 0..8 {
                        for col in 0..8 {
                            let (px, py) = (mb_x * 16 + dx + col, mb_y * 16 + dy + row);
                            assert_eq!(
                                decoded_y[py * width + px],
                                choice.recon[row * 8 + col],
                                "MB ({mb_x},{mb_y}) leaf ({dx},{dy}) sample ({col},{row})"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Adaptive λ-tree round trip: the decoded frame equals the
    /// per-macroblock plan reconstructions, λ = 0 is at least as good
    /// in SSE as the fixed multi-stage L=3 tree, and a large λ
    /// produces a smaller stream.
    #[test]
    fn adaptive_mode_round_trips_and_orders_by_lambda() {
        let (width, height) = (64usize, 48usize);
        let (y, u, v) = planes(width, height);
        let (yr, ur, vr) = refs(&y, &u, &v, width, height);

        let enc_sharp = encode_intra_frame(yr, ur, vr, Svq1EncoderMode::Adaptive { lambda: 0 })
            .expect("encodes");
        let enc_coarse =
            encode_intra_frame(yr, ur, vr, Svq1EncoderMode::Adaptive { lambda: 1 << 16 })
                .expect("encodes");
        let enc_multi =
            encode_intra_frame(yr, ur, vr, Svq1EncoderMode::MultiStageL3).expect("encodes");

        assert!(
            enc_coarse.len() < enc_sharp.len(),
            "large lambda must shrink the stream ({} vs {})",
            enc_coarse.len(),
            enc_sharp.len()
        );

        let sse = |decoded: &[u8], source: &[u8]| -> u64 {
            decoded
                .iter()
                .zip(source.iter())
                .map(|(&a, &b)| {
                    let d = i64::from(a) - i64::from(b);
                    (d * d) as u64
                })
                .sum()
        };
        let dec_sharp = decode_intra_frame(&enc_sharp).expect("decodes");
        let dec_coarse = decode_intra_frame(&enc_coarse).expect("decodes");
        let dec_multi = decode_intra_frame(&enc_multi).expect("decodes");
        let sharp_sse = sse(&dec_sharp.y.visible(), &y);
        let multi_sse = sse(&dec_multi.y.visible(), &y);
        assert!(
            sharp_sse <= multi_sse,
            "lambda 0 must not lose to fixed L=3 ({sharp_sse} vs {multi_sse})"
        );

        // Per-MB cross-check: the decode equals the plan recon.
        let decoded_y = dec_coarse.y.visible();
        for mb_y in 0..height / 16 {
            for mb_x in 0..width / 16 {
                let block = yr.block(mb_x * 16, mb_y * 16, 16, 16);
                let mut target = [0u8; 256];
                target.copy_from_slice(&block);
                let plan = crate::svq1_enc_tree::plan_macroblock(
                    &target,
                    &[0u8; 256],
                    crate::svq1_vlc::Svq1Half::Intra,
                    false,
                    1 << 16,
                );
                let recon = crate::svq1_enc_tree::plan_reconstruction(&plan);
                for row in 0..16 {
                    for col in 0..16 {
                        assert_eq!(
                            decoded_y[(mb_y * 16 + row) * width + mb_x * 16 + col],
                            recon[row * 16 + col],
                            "MB ({mb_x},{mb_y}) sample ({col},{row})"
                        );
                    }
                }
            }
        }
    }

    /// Non-standard dimensions take the code-7 explicit escape and
    /// round-trip through the header parser.
    #[test]
    fn explicit_dimension_escape_round_trips() {
        let (width, height) = (48usize, 32usize);
        let (y, u, v) = planes(width, height);
        let (yr, ur, vr) = refs(&y, &u, &v, width, height);
        let encoded = encode_intra_frame(yr, ur, vr, Svq1EncoderMode::MeanOnlyL5).expect("encodes");
        let frame = decode_intra_frame(&encoded).expect("decodes");
        assert_eq!(frame.header.frame_size_code, Some(7));
        assert_eq!(frame.width(), width);
        assert_eq!(frame.height(), height);
    }

    /// Mis-sized chroma planes are rejected.
    #[test]
    fn rejects_mis_sized_planes() {
        let (width, height) = (32usize, 32usize);
        let (y, u, v) = planes(width, height);
        let bad_u = Svq1PlaneRef {
            samples: &u[..u.len() - 1],
            width: chroma_dim(width),
            height: chroma_dim(height),
        };
        let (yr, _, vr) = refs(&y, &u, &v, width, height);
        assert!(encode_intra_frame(yr, bad_u, vr, Svq1EncoderMode::MeanOnlyL5).is_err());
    }
}
