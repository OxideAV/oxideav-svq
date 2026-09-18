//! The SVQ3 intra-picture edge filter —
//! `docs/video/svq3/spec/09-intra-picture-edge-filter.md`.
//!
//! After every macroblock of an **I picture** has been reconstructed
//! the decoder smooths every 4-sample block edge of each plane by up
//! to `limit[quantiser]` (`tables/09`, 0 below quantiser 11); the
//! smoothed picture is what is displayed and what the following P
//! pictures reference (§1). P and B pictures are not filtered.
//!
//! * **Strength** (§2): luma uses the luma quantiser in force after
//!   the picture's last macroblock, each chroma plane the remapped
//!   chroma index of spec/04 §3 derived from the same value; limit 0
//!   disables the pass for that plane.
//! * **Geometry** (§3): over the macroblock-aligned plane, first every
//!   vertical edge `x = 4k` (`k = 1 … ⌊(w − 1)/4⌋`) for every row,
//!   then every horizontal edge `y = 4k` for every column. Every edge
//!   at 4-sample pitch is filtered — no macroblock / quadrant / block
//!   distinction, no pattern or activity test; the picture border is
//!   not an edge; the second sweep reads the first sweep's output.
//! * **Edge operation** (§4), on `p1 p0 | q0 q1`:
//!   `delta = trunc((4·(q0 − p0) + (p1 − q1)) / 8)`, clipped to
//!   `±limit`; `p0' = clip255(p0 + delta)`, `q0' = clip255(q0 − delta)`;
//!   `p1` and `q1` are read only.
//!
//! The fixtures' black-box `expected.yuv` omits the pass (§6); with it
//! disabled this decoder reproduces that file, and with it enabled the
//! component's own pictures (`Svq3DecodeOptions::intra_edge_filter`).

use crate::svq3_dequant::chroma_quantiser_index;
use crate::svq3_picture::Svq3Picture;
use crate::svq3_tables::SVQ3_INTRA_EDGE_FILTER_LIMIT;

/// The filter limit for quantiser index `q` (`tables/09`, spec/09 §2).
///
/// # Panics
///
/// Panics if `q > 31`.
#[must_use]
pub fn edge_filter_limit(q: u32) -> u8 {
    SVQ3_INTRA_EDGE_FILTER_LIMIT[q as usize]
}

/// The truncating-toward-zero division by 8 of spec/09 §4, in the
/// decoder's form `(v + ((v >> 31) & 7)) >> 3`.
#[inline]
const fn div8_trunc(v: i32) -> i32 {
    (v + ((v >> 31) & 7)) >> 3
}

/// Filter one edge `p1 p0 | q0 q1` with `limit` (spec/09 §4), returning
/// the new `(p0, q0)`.
#[must_use]
pub const fn filter_edge(p1: u8, p0: u8, q0: u8, q1: u8, limit: u8) -> (u8, u8) {
    let (p1, p0, q0, q1) = (p1 as i32, p0 as i32, q0 as i32, q1 as i32);
    let limit = limit as i32;
    let mut delta = div8_trunc(4 * (q0 - p0) + (p1 - q1));
    if delta > limit {
        delta = limit;
    } else if delta < -limit {
        delta = -limit;
    }
    (clip255(p0 + delta), clip255(q0 - delta))
}

const fn clip255(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v as u8
    }
}

/// Run the two sweeps of spec/09 §3 over a `width × height` row-major
/// plane with the given limit (a limit of 0 returns immediately).
pub fn filter_plane(samples: &mut [u8], width: usize, height: usize, limit: u8) {
    if limit == 0 || width == 0 || height == 0 {
        return;
    }
    debug_assert_eq!(samples.len(), width * height);
    // Vertical edges first: x = 4k, k = 1 … ⌊(w − 1)/4⌋, every row. On
    // the macroblock-aligned planes the decoder filters, q1 = x + 1 is
    // always inside the plane; for other widths (spec/09 §7: not
    // established) an edge whose q1 would fall outside is skipped.
    let last_x_edge = (width - 1) / 4;
    for y in 0..height {
        let row = &mut samples[y * width..(y + 1) * width];
        for k in 1..=last_x_edge {
            let x = 4 * k;
            if x + 1 >= width {
                break;
            }
            let (p0, q0) = filter_edge(row[x - 2], row[x - 1], row[x], row[x + 1], limit);
            row[x - 1] = p0;
            row[x] = q0;
        }
    }
    // Horizontal edges second: y = 4k, k = 1 … ⌊(h − 1)/4⌋, every column,
    // reading the vertically filtered samples.
    let last_y_edge = (height - 1) / 4;
    for k in 1..=last_y_edge {
        let y = 4 * k;
        if y + 1 >= height {
            break;
        }
        for x in 0..width {
            let i = |row: usize| row * width + x;
            let (p0, q0) = filter_edge(
                samples[i(y - 2)],
                samples[i(y - 1)],
                samples[i(y)],
                samples[i(y + 1)],
                limit,
            );
            samples[i(y - 1)] = p0;
            samples[i(y)] = q0;
        }
    }
}

/// Filter a reconstructed I picture in place (spec/09 §1–§2): the luma
/// plane with `limit[qp]` and both chroma planes with
/// `limit[chroma_quantiser_index(qp)]`, `qp` being the luma quantiser
/// in force after the picture's last macroblock.
///
/// # Panics
///
/// Panics if `qp > 31`.
pub fn filter_intra_picture(picture: &mut Svq3Picture, qp: u32) {
    let luma_limit = edge_filter_limit(qp);
    let chroma_limit = edge_filter_limit(chroma_quantiser_index(qp));
    let (lw, lh) = (picture.luma_width(), picture.luma_height());
    let (cw, ch) = (picture.chroma_width(), picture.chroma_height());
    filter_plane(picture.luma_mut(), lw, lh, luma_limit);
    filter_plane(picture.cb_mut(), cw, ch, chroma_limit);
    filter_plane(picture.cr_mut(), cw, ch, chroma_limit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_table_and_gate() {
        // spec/09 §2: 0…10 → 0 (pass disabled), 11…15 → 1, 31 → 5.
        assert_eq!(edge_filter_limit(0), 0);
        assert_eq!(edge_filter_limit(10), 0);
        assert_eq!(edge_filter_limit(11), 1);
        assert_eq!(edge_filter_limit(13), 1);
        assert_eq!(edge_filter_limit(16), 2);
        assert_eq!(edge_filter_limit(31), 5);
        // Chroma of quantiser 13 remaps to 13 → limit 1; of 31 → 25 → 4.
        assert_eq!(edge_filter_limit(chroma_quantiser_index(13)), 1);
        assert_eq!(edge_filter_limit(chroma_quantiser_index(31)), 4);
    }

    #[test]
    fn truncating_division() {
        assert_eq!(div8_trunc(9), 1);
        assert_eq!(div8_trunc(-9), -1);
        assert_eq!(div8_trunc(-8), -1);
        assert_eq!(div8_trunc(-7), 0);
        assert_eq!(div8_trunc(7), 0);
        assert_eq!(div8_trunc(-16), -2);
    }

    #[test]
    fn worked_example_edges() {
        // spec/09 §5: `2 2 | 5 5` → delta = (12 − 3)/8 = 1 → `3 | 4`;
        // `2 2 | 4 4` (already filtered) → delta = (8 − 2)/8 = 0.
        assert_eq!(filter_edge(2, 2, 5, 5, 1), (3, 4));
        assert_eq!(filter_edge(2, 2, 4, 4, 1), (2, 4));
        // §4: with limit 1 the step across a flat edge must be ≥ 3.
        assert_eq!(filter_edge(10, 10, 12, 12, 1), (10, 12));
        assert_eq!(filter_edge(10, 10, 13, 13, 1), (11, 12));
        // The clip to ±limit and the 0…255 clamp.
        assert_eq!(filter_edge(0, 0, 255, 255, 5), (5, 250));
        assert_eq!(filter_edge(255, 255, 0, 0, 5), (250, 5));
        assert_eq!(filter_edge(255, 255, 0, 0, 0), (255, 0));
        // 4·(1 − 0) + (0 − 255) = −251 → −31 → clipped to −5.
        assert_eq!(filter_edge(0, 0, 1, 255, 5), (0, 6));
    }

    #[test]
    fn worked_example_area() {
        // spec/09 §5: block (24, 8) reconstructs to the constant 5 on a
        // surround of 2 — its three quadrant siblings, DC-predicted from
        // it, reconstruct to 4 (which is why the tape shows the x = 28
        // and y = 12 edges rewriting unchanged values). After the
        // vertical then the horizontal sweep at limit 1 the area
        // x = 23…27, y = 7…11 holds row 7 `2 3 3 3` (columns 24…27),
        // column 23 `3 3 3 3` (rows 8…11), and block (24, 8) a 4-valued
        // edge row and column around a 5-valued interior.
        let (w, h) = (32usize, 16usize);
        let mut plane = vec![2u8; w * h];
        for y in 8..16 {
            for x in 24..32 {
                plane[y * w + x] = if y < 12 && x < 28 { 5 } else { 4 };
            }
        }
        filter_plane(&mut plane, w, h, 1);
        let at = |x: usize, y: usize| plane[y * w + x];
        assert_eq!([at(24, 7), at(25, 7), at(26, 7), at(27, 7)], [2, 3, 3, 3]);
        assert_eq!([at(23, 8), at(23, 9), at(23, 10), at(23, 11)], [3, 3, 3, 3]);
        for y in 8..12 {
            assert_eq!(at(24, y), 4, "column 24 row {y}");
        }
        for x in 25..28 {
            assert_eq!(at(x, 8), 4, "row 8 column {x}");
        }
        for y in 9..12 {
            for x in 25..28 {
                assert_eq!(at(x, y), 5, "interior ({x},{y})");
            }
        }
        // Column 24, row 7 stays 2: the horizontal sweep at (24, 8) saw
        // `2 2 | 4 4` (already filtered) and left it; the x = 28 and
        // y = 12 edges see `5 5 | 4 4` and change nothing.
        assert_eq!(at(24, 7), 2);
        assert_eq!(at(27, 9), 5);
        assert_eq!(at(28, 9), 4);
        assert_eq!(at(25, 11), 5);
        assert_eq!(at(25, 12), 4);
    }

    #[test]
    fn non_aligned_plane_sizes_do_not_read_past_the_plane() {
        // Found by fuzz/svq3_filter_mc: a height (or width) of 1 mod 4
        // puts the last edge's q1 outside the plane; that edge is
        // skipped (spec/09 §7 leaves non-16-multiple sizes open; the
        // decoder itself only filters macroblock-aligned canvases).
        for (w, h) in [(5usize, 5usize), (9, 1), (1, 9), (13, 6), (8, 5)] {
            let mut plane: Vec<u8> = (0..(w * h)).map(|i| (i * 37 % 256) as u8).collect();
            let before = plane.clone();
            filter_plane(&mut plane, w, h, 5);
            for (a, b) in before.iter().zip(&plane) {
                assert!(a.abs_diff(*b) <= 10);
            }
        }
    }

    #[test]
    fn limit_zero_and_flat_planes_are_untouched() {
        let mut plane: Vec<u8> = (0..64u8).collect();
        let before = plane.clone();
        filter_plane(&mut plane, 8, 8, 0);
        assert_eq!(plane, before);
        let mut flat = vec![77u8; 16 * 16];
        filter_plane(&mut flat, 16, 16, 5);
        assert!(flat.iter().all(|&s| s == 77));
    }

    #[test]
    fn picture_filter_uses_luma_and_chroma_limits() {
        // 32×32 picture at quantiser 13: luma and chroma limits 1. A
        // step of 3 across the x = 4 edge on every plane is smoothed.
        let mut pic = Svq3Picture::new(2, 2);
        let fill = |plane: &mut [u8], w: usize| {
            for (i, s) in plane.iter_mut().enumerate() {
                *s = if i % w < 4 { 100 } else { 103 };
            }
        };
        fill(pic.luma_mut(), 32);
        fill(pic.cb_mut(), 16);
        fill(pic.cr_mut(), 16);
        filter_intra_picture(&mut pic, 13);
        assert_eq!(pic.luma_sample(3, 0), 101);
        assert_eq!(pic.luma_sample(4, 0), 102);
        assert_eq!(pic.cb_sample(3, 0), 101);
        assert_eq!(pic.cr_sample(4, 0), 102);
        // Quantiser 5: nothing happens.
        let mut pic2 = Svq3Picture::new(2, 2);
        for (i, s) in pic2.luma_mut().iter_mut().enumerate() {
            *s = if i % 32 < 4 { 100 } else { 103 };
        }
        filter_intra_picture(&mut pic2, 5);
        assert_eq!(pic2.luma_sample(3, 0), 100);
    }
}
