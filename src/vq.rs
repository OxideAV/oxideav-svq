//! Body decoding — hierarchical multistage VQ walk + flat-fill fallback.
//!
//! Implements the syntactic skeleton from §5–§7 of the trace doc:
//!
//! * Three planes in `Y, U, V` order, each at 4:1:0 sampling
//!   (chroma is 1/4 in each dimension).
//! * Per-plane raster of 16x16 macroblocks over the 16-aligned plane
//!   size; the cropped output is the declared `width × height`.
//! * Per-MB recursion via [`Quadtree`], from level 5 down to level 0
//!   along the alternating axis (horizontal at odd levels, vertical at
//!   even).
//!
//! ## Pixel rendering — round-1 flat-fill fallback
//!
//! `docs/video/svq1/svq1-trace-reverse-engineering.md` is explicit
//! that the per-level multistage VLC tables and the static codebook
//! bytes are not in the document and "must be reverse-engineered
//! from a reference decoder". Workspace policy bars us from copying
//! those tables out of any third-party source.
//!
//! Until those tables land in `docs/`, we cannot parse the
//! body bits semantically. This module therefore implements the
//! **flat-fill fallback** path the trace doc anticipates: every plane
//! is filled with the per-component midpoint (`128` for luma and
//! chroma in `Yuv420P` units). The decoder pipeline is otherwise
//! complete — packet → header parse → flat-fill body → upsample chroma
//! 2x to `Yuv420P` → emit `VideoFrame`.
//!
//! When the codebook arrives, `decode_plane_flat` will be replaced by
//! `decode_plane_quadtree` and the recursion in [`Quadtree::walk`]
//! will start consuming bits.

use crate::codebook::{LEAF_DIMS, MAX_LEVEL};

/// Round a positive integer up to the nearest multiple of `align`.
#[inline]
pub fn align_up(x: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (x + align - 1) & !(align - 1)
}

/// Per-plane padded dimensions used during the body walk.
///
/// SVQ1 always rasterises in 16x16 MBs over the 16-aligned plane
/// extent (§5 of the trace doc), and the chroma planes are forced to
/// at least one full 16x16 MB by the `FFALIGN(W/4, 16) >= 16` rule.
#[derive(Clone, Copy, Debug)]
pub struct PlaneDims {
    /// Output width (cropped).
    pub width: usize,
    /// Output height (cropped).
    pub height: usize,
    /// Width rounded up to multiple of 16 (the MB raster width).
    pub padded_w: usize,
    /// Height rounded up to multiple of 16.
    pub padded_h: usize,
}

impl PlaneDims {
    pub fn for_luma(w: u16, h: u16) -> Self {
        let width = w as usize;
        let height = h as usize;
        Self {
            width,
            height,
            padded_w: align_up(width, 16),
            padded_h: align_up(height, 16),
        }
    }

    pub fn for_chroma(w: u16, h: u16) -> Self {
        // 4:1:0 → chroma plane is W/4 × H/4, with the MB raster
        // forced to at least one 16x16 MB by FFALIGN.
        let width = (w as usize).div_ceil(4);
        let height = (h as usize).div_ceil(4);
        Self {
            width,
            height,
            padded_w: align_up(width.max(1), 16),
            padded_h: align_up(height.max(1), 16),
        }
    }

    /// Raster of MBs in (x, y) order.
    pub fn mbs(&self) -> impl Iterator<Item = (usize, usize)> {
        let cols = self.padded_w / 16;
        let rows = self.padded_h / 16;
        (0..rows).flat_map(move |r| (0..cols).map(move |c| (c * 16, r * 16)))
    }
}

/// Hierarchical quad-tree iterator skeleton for one 16x16 MB.
///
/// Once the codebook lands, `walk` will drive the bit-reader through
/// the split-flag / multistage_count / mean / codebook_idx[] sequence
/// described in §7.1 of the trace doc, calling `visit` at each leaf
/// with the leaf's level and pixel-index range. For the round-1
/// flat-fill decoder we expose this as a simple level-0 raster scan
/// over the MB so callers can blit a precomputed mean colour into the
/// padded plane.
#[derive(Clone, Copy, Debug)]
pub struct Quadtree;

impl Quadtree {
    /// Visit every leaf of an MB at level `MAX_LEVEL` as the trivial
    /// "one leaf at the root" tree — i.e. fill the whole 16x16 area
    /// with the supplied mean. This is what the body decoder uses
    /// today.
    pub fn fill_mb(plane: &mut [u8], stride: usize, mb_x: usize, mb_y: usize, value: u8) {
        let (w, h) = LEAF_DIMS[MAX_LEVEL as usize];
        for dy in 0..h {
            let y = mb_y + dy;
            let row = y * stride;
            for dx in 0..w {
                plane[row + mb_x + dx] = value;
            }
        }
    }
}

/// Decode one plane in flat-fill mode. Allocates `padded_w * padded_h`
/// bytes filled with `value`. The caller is responsible for cropping.
pub fn decode_plane_flat(dims: &PlaneDims, value: u8) -> Vec<u8> {
    let mut buf = vec![value; dims.padded_w * dims.padded_h];
    // The fill_mb helper is exercised here so the function signature
    // is "alive" — every MB gets touched even though the value is the
    // same. This keeps the per-MB raster code path covered and ready
    // to be repurposed when the VQ walker replaces flat-fill.
    for (mb_x, mb_y) in dims.mbs() {
        Quadtree::fill_mb(&mut buf, dims.padded_w, mb_x, mb_y, value);
    }
    buf
}

/// Crop a padded plane back to its declared (`width` × `height`).
pub fn crop_plane(padded: &[u8], dims: &PlaneDims) -> Vec<u8> {
    let mut out = Vec::with_capacity(dims.width * dims.height);
    for row in 0..dims.height {
        let start = row * dims.padded_w;
        out.extend_from_slice(&padded[start..start + dims.width]);
    }
    out
}

/// Upsample a 4:1:0 chroma plane (W/4 × H/4) to 4:2:0 (W/2 × H/2) by
/// pixel duplication. The output's extent is computed from the luma
/// dims: `(luma_w+1)/2 × (luma_h+1)/2`. We use simple nearest-
/// neighbour 2x replication to keep the code dependency-free.
///
/// `src` is the **cropped** 4:1:0 plane (no padding) of size
/// `src_dims.width × src_dims.height`.
pub fn upsample_chroma_410_to_420(
    src: &[u8],
    src_dims: &PlaneDims,
    luma_w: u16,
    luma_h: u16,
) -> (Vec<u8>, usize, usize) {
    let dst_w = (luma_w as usize).div_ceil(2);
    let dst_h = (luma_h as usize).div_ceil(2);
    let mut dst = vec![128u8; dst_w * dst_h];
    if src_dims.width == 0 || src_dims.height == 0 {
        return (dst, dst_w, dst_h);
    }
    for dy in 0..dst_h {
        let sy = (dy / 2).min(src_dims.height - 1);
        for dx in 0..dst_w {
            let sx = (dx / 2).min(src_dims.width - 1);
            dst[dy * dst_w + dx] = src[sy * src_dims.width + sx];
        }
    }
    (dst, dst_w, dst_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_dims_round_up_to_mb() {
        let d = PlaneDims::for_luma(176, 144);
        assert_eq!((d.padded_w, d.padded_h), (176, 144));
        let d = PlaneDims::for_luma(160, 120);
        assert_eq!((d.padded_w, d.padded_h), (160, 128));
    }

    #[test]
    fn chroma_min_one_mb() {
        let d = PlaneDims::for_chroma(40, 32);
        // W/4=10, H/4=8 → padded to 16x16.
        assert_eq!((d.padded_w, d.padded_h), (16, 16));
    }

    #[test]
    fn flat_fill_padded() {
        let d = PlaneDims::for_luma(160, 120);
        let buf = decode_plane_flat(&d, 100);
        assert_eq!(buf.len(), 160 * 128);
        assert!(buf.iter().all(|&b| b == 100));
    }

    #[test]
    fn crop_drops_padding() {
        let d = PlaneDims::for_luma(160, 120);
        let buf = decode_plane_flat(&d, 100);
        let crop = crop_plane(&buf, &d);
        assert_eq!(crop.len(), 160 * 120);
    }

    #[test]
    fn chroma_upsample_doubles_each_axis() {
        let d = PlaneDims::for_chroma(176, 144);
        let padded = decode_plane_flat(&d, 128);
        let cropped = crop_plane(&padded, &d);
        let (out, w, h) = upsample_chroma_410_to_420(&cropped, &d, 176, 144);
        assert_eq!(w, 88);
        assert_eq!(h, 72);
        assert_eq!(out.len(), w * h);
    }
}
