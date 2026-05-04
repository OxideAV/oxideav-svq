//! Body decoding — hierarchical multistage VQ walk for SVQ1.
//!
//! Implements §5–§7 of `docs/video/svq1/svq1-trace-reverse-engineering.md`:
//!
//! * Three planes in `Y, U, V` order, each at 4:1:0 sampling
//!   (chroma is 1/4 in each dimension).
//! * Per-plane raster of 16x16 macroblocks over the 16-aligned plane
//!   size; the cropped output is the declared `width × height`.
//! * Per-MB recursion via [`decode_intra_block`], from level 5 down to
//!   level 0 along the alternating axis (height-halved at odd levels,
//!   width-halved at even levels per
//!   [`super::codebook::split_halves_height`]).
//!
//! ## Per-leaf reconstruction (intra)
//!
//! At each leaf, after `multistage_count` and (when `count >= 0`) the
//! `mean` are decoded, the leaf's pixel buffer is initialised to the
//! mean. If `count >= 1`, then `count` codebook entries are added to
//! the buffer (each indexed by 4 bits read raw from the stream). The
//! result is clipped to `[0, 255]` and blitted into the plane.
//!
//! The walker is **depth-first**: when a parent node decides to split
//! it recurses fully into the first child, then the second. This is
//! the only walk order consistent with the recursive formulation in
//! §10 of the trace doc — "if split, recurse twice on level L-1
//! along the alternating axis" — and also matches the bit-position
//! deltas the trace records show (the `bit_pos=` increments after a
//! split-flag are entirely consumed by the first child's leaf chain
//! before the second child's split-flag is read).

use oxideav_core::bits::BitReader;
use oxideav_core::{Error, Result};

use super::codebook::{
    intra_codebook_for_level, max_stages_at_level, split_halves_height, LEAF_DIMS, MAX_LEVEL,
};
use super::tables::{INTRA_MEAN_VLC, INTRA_MULTISTAGE_VLC};
use super::vlc::Vlc;

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

/// Bundle of pre-built VLC decoders. Building a `Vlc` walks the
/// table once to sort by length; doing it per-frame is wasteful, so
/// we build them once per decode call and pass by reference into the
/// recursion.
pub struct VlcBundle {
    pub intra_multistage: [Vlc; 6],
    pub intra_mean: Vlc,
}

impl VlcBundle {
    pub fn build() -> Self {
        Self {
            intra_multistage: [
                Vlc::new(INTRA_MULTISTAGE_VLC[0]),
                Vlc::new(INTRA_MULTISTAGE_VLC[1]),
                Vlc::new(INTRA_MULTISTAGE_VLC[2]),
                Vlc::new(INTRA_MULTISTAGE_VLC[3]),
                Vlc::new(INTRA_MULTISTAGE_VLC[4]),
                Vlc::new(INTRA_MULTISTAGE_VLC[5]),
            ],
            intra_mean: Vlc::new(INTRA_MEAN_VLC),
        }
    }
}

/// Decode one intra macroblock at the given (x, y) into `plane` of
/// row-stride `stride`. Recursively walks levels 5 → 0 along the
/// alternating split axis.
///
/// Returns `Err` on invalid bitstream (e.g. stage count > 0 at level
/// 4 or 5, mean VLC mismatch, etc.).
pub fn decode_intra_mb(
    br: &mut BitReader<'_>,
    vlcs: &VlcBundle,
    plane: &mut [u8],
    stride: usize,
    mb_x: usize,
    mb_y: usize,
) -> Result<()> {
    let (w, h) = LEAF_DIMS[MAX_LEVEL as usize];
    decode_intra_block(br, vlcs, plane, stride, mb_x, mb_y, w, h, MAX_LEVEL)
}

/// Recursive intra-block decoder. `(x, y, w, h)` is the sub-block's
/// top-left + size in plane coordinates; `level` is its level
/// in `0..=MAX_LEVEL`.
fn decode_intra_block(
    br: &mut BitReader<'_>,
    vlcs: &VlcBundle,
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    level: u8,
) -> Result<()> {
    if level > 0 {
        let split = br.read_u1()? != 0;
        if split {
            // Per §7: split along an alternating axis. At odd L the
            // split halves height (top/bottom). At even L it halves
            // width (left/right). Recurse depth-first: first child
            // first, second child second.
            if split_halves_height(level) {
                let half_h = h / 2;
                decode_intra_block(br, vlcs, plane, stride, x, y, w, half_h, level - 1)?;
                decode_intra_block(br, vlcs, plane, stride, x, y + half_h, w, half_h, level - 1)?;
            } else {
                let half_w = w / 2;
                decode_intra_block(br, vlcs, plane, stride, x, y, half_w, h, level - 1)?;
                decode_intra_block(br, vlcs, plane, stride, x + half_w, y, half_w, h, level - 1)?;
            }
            return Ok(());
        }
    }
    // Leaf at this level.
    decode_intra_leaf(br, vlcs, plane, stride, x, y, w, h, level)
}

/// Decode and reconstruct a leaf:
///
/// 1. `count = vlc(intra_multistage[level])` — value in `0..=6` (intra
///    stop value is `-1` but the trace doc reports `count == -1` is
///    not used in INTRA paths in our corpus; we treat it as
///    "stages = 0", same as count = 0; alternatively as an error.
/// 2. `mean = vlc(intra_mean)` (8-bit unsigned).
/// 3. If `count >= 1`: read `count × u(4)` codebook indices; add the
///    corresponding stage entries to the leaf buffer.
/// 4. Clip to `[0, 255]` and blit to the plane.
fn decode_intra_leaf(
    br: &mut BitReader<'_>,
    vlcs: &VlcBundle,
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    level: u8,
) -> Result<()> {
    let count_raw = vlcs.intra_multistage[level as usize].decode(br)?;
    // The trace doc (§7 lines 416–431) empirically claims `count > 0`
    // never occurs at L=4 (16×8) or L=5 (16×16). Round 3 verified
    // bit-by-bit on a real ffmpeg-encoded testsrc-176×144 I-frame
    // (see `tests/trace_first_mbs.rs`) that this claim is wrong:
    // the FFmpeg encoder really does emit e.g. `count=2` at L=5 for
    // non-uniform MBs. The corresponding L=4/L=5 VQ codebooks (per
    // §7.2 the shape is 6 stages × 16 entries ×
    // `pixels_at_level(L)` signed bytes) are NOT in §14 of the trace
    // doc. Until they land, this leaf path consumes the codebook
    // indices below to keep the bit-reader aligned but skips the
    // additive contribution, leaving the leaf at mean-only fill.
    // This costs roughly 10 dB of testsrc PSNR; once the codebooks
    // are transcribed `intra_codebook_for_level(L>=4)` returns
    // `Some(_)` and the additive path takes over automatically.
    //
    // `count == -1` is the INTER-only "skip" sentinel and should
    // never appear in INTRA — if it does, the bitstream is corrupt
    // upstream of this leaf, but we still want to keep the bit
    // alignment of the rest of the frame, so we treat it as
    // count == 0 (mean-only) rather than erroring.
    let count = count_raw.max(0) as usize;
    let mean = vlcs.intra_mean.decode(br)?;
    if !(0..=255).contains(&mean) {
        return Err(Error::invalid(
            "svq1: intra mean VLC produced out-of-range value",
        ));
    }

    let pixels = w * h;
    let mut buf = vec![mean; pixels];
    if count > 0 {
        if let Some(cb) = intra_codebook_for_level(level) {
            debug_assert_eq!(cb.pixels(), pixels);
            for stage in 0..count {
                let idx = br.read_u32(4)? as usize;
                cb.add_into(stage, idx, &mut buf);
            }
        } else {
            // Levels 4 and 5: codebook not in trace doc. Consume the
            // codebook-index bits (4 per stage) so the bit-reader stays
            // aligned for the rest of the frame, but skip the additive
            // contribution.
            for _ in 0..count {
                let _ = br.read_u32(4)?;
            }
        }
    }
    let _ = max_stages_at_level; // kept around for callers that want a strict-mode validator

    // Clip + blit.
    for dy in 0..h {
        let row_off = (y + dy) * stride + x;
        for dx in 0..w {
            let v = buf[dy * w + dx];
            plane[row_off + dx] = v.clamp(0, 255) as u8;
        }
    }
    Ok(())
}

/// Decode one full intra plane: walks every MB in raster order and
/// decodes it as a quad-tree. Returns the padded plane buffer of size
/// `padded_w * padded_h` bytes.
pub fn decode_plane_intra(
    br: &mut BitReader<'_>,
    vlcs: &VlcBundle,
    dims: &PlaneDims,
) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; dims.padded_w * dims.padded_h];
    for (mb_x, mb_y) in dims.mbs() {
        decode_intra_mb(br, vlcs, &mut buf, dims.padded_w, mb_x, mb_y)?;
    }
    Ok(buf)
}

/// Decode one plane in flat-fill mode. Allocates `padded_w * padded_h`
/// bytes filled with `value`. Used for P-frames in round 2 (motion
/// comp lands in round 3).
pub fn decode_plane_flat(dims: &PlaneDims, value: u8) -> Vec<u8> {
    vec![value; dims.padded_w * dims.padded_h]
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

    /// Tiny smoke test: synthesise an MB encoded as a single split=0
    /// at level 5 with `count=0`, `mean=128`. The full MB should be
    /// flat-fill at 128.
    #[test]
    fn decode_single_mean_mb() {
        use oxideav_core::bits::BitWriter;

        let mut bw = BitWriter::new();
        // split=0 at level 5
        bw.write_bit(false);
        // multistage VLC for level 5, value=0 → from §14.2, code "1"
        bw.write_u32(0b1, 1);
        // mean VLC, value=128 → from §14.4 row 128: "01001001" length 8
        bw.write_u32(0b0100_1001, 8);
        // pad
        bw.write_u32(0, 16);
        let bytes = bw.into_bytes();

        let vlcs = VlcBundle::build();
        let mut br = BitReader::new(&bytes);
        let mut plane = vec![0u8; 16 * 16];
        decode_intra_mb(&mut br, &vlcs, &mut plane, 16, 0, 0).unwrap();
        for &v in &plane {
            assert_eq!(v, 128, "expected flat 128, got {v}");
        }
    }

    /// Verify the alternating split axis with two-level recursion: at
    /// level 5 split → two level-4 leaves stacked top/bottom; each at
    /// level-4 must flat-fill (split=0, count=0, mean) since L=4 has
    /// no codebooks.
    #[test]
    fn decode_split_top_bottom_at_l5() {
        use oxideav_core::bits::BitWriter;

        let mut bw = BitWriter::new();
        // L=5 split=1
        bw.write_bit(true);
        // First child (top half, level 4): split=0, count=0, mean=100
        bw.write_bit(false);
        // multistage VLC for level 4, value=0 → "1" (length 1)
        bw.write_u32(0b1, 1);
        // mean=100 → §14.4 row 100: "1010010" length 7
        bw.write_u32(0b101_0010, 7);
        // Second child (bottom half, level 4): split=0, count=0, mean=200
        bw.write_bit(false);
        bw.write_u32(0b1, 1);
        // mean=200 → §14.4 row 200: "000110001" length 9
        bw.write_u32(0b0_0011_0001, 9);
        bw.write_u32(0, 16);
        let bytes = bw.into_bytes();

        let vlcs = VlcBundle::build();
        let mut br = BitReader::new(&bytes);
        let mut plane = vec![0u8; 16 * 16];
        decode_intra_mb(&mut br, &vlcs, &mut plane, 16, 0, 0).unwrap();
        // Top 8 rows = 100, bottom 8 rows = 200.
        for y in 0..8 {
            for x in 0..16 {
                assert_eq!(plane[y * 16 + x], 100, "top half y={y} x={x}");
            }
        }
        for y in 8..16 {
            for x in 0..16 {
                assert_eq!(plane[y * 16 + x], 200, "bottom half y={y} x={x}");
            }
        }
    }
}
