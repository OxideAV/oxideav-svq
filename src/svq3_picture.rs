//! SVQ3 picture-plane assembly — the full-frame Y/Cb/Cr canvas the
//! per-macroblock reconstruction units write into.
//!
//! [`crate::svq3_recon`] reconstructs **one macroblock at a time** into
//! standalone [`LumaMacroblock`] / [`ChromaPlane`] carriers, each of which
//! reads its out-of-macroblock neighbour rows/columns (the pixels of the
//! macroblock above / to the left) from explicit `above` / `leftcol` /
//! `corner` fields the caller must populate. This module owns the
//! **picture-level geometry** that ties those per-MB units into a whole
//! decoded picture:
//!
//! * [`Svq3Picture`] — three contiguous row-major sample planes (one luma,
//!   two chroma) sized to the macroblock grid, with the `4:2:0`
//!   luma-to-chroma 2× subsample relationship the wiki's
//!   §"Macroblock layer" fixes ("chroma DCs are stored in 2×2 blocks";
//!   each macroblock owns a 16×16 luma region and an 8×8 region per chroma
//!   plane).
//! * **Neighbour binding** ([`Svq3Picture::bind_luma_neighbours`] /
//!   [`Svq3Picture::bind_chroma_neighbours`]) — populate a per-MB carrier's
//!   `above` / `leftcol` / `corner` + availability flags from the
//!   **already-reconstructed** pixels of the picture canvas at a macroblock
//!   raster position. Because macroblocks are decoded in raster order
//!   (left-to-right, top-to-bottom, per the wiki §"Macroblock layer"), the
//!   above row and left column of any macroblock are always reconstructed
//!   by the time that macroblock is processed.
//! * **Blit-back** ([`Svq3Picture::blit_luma`] / [`Svq3Picture::blit_chroma`])
//!   — copy a reconstructed per-MB carrier's `samples` into the picture
//!   canvas at the macroblock's pixel origin, so subsequent macroblocks
//!   read them as neighbours.
//!
//! All geometry here is wire-format-independent: it threads pixels between
//! the picture canvas and the per-MB carriers using only the macroblock
//! raster ordering and the `4:2:0` subsample relationship, both of which
//! are pinned by the wiki §"Macroblock layer". It does **not** depend on
//! the CBP `me(v)` decode or the separate-DC-luma branch (the two
//! remaining SVQ3 docs gaps), which only govern *which residual blocks a
//! macroblock carries* — not where a reconstructed macroblock lands in the
//! picture.

use crate::svq3::Svq3MacroblockPosition;
use crate::svq3_recon::{ChromaPlane, LumaMacroblock, CHROMA_PLANE_DIM, MB_LUMA_DIM};

/// A full decoded SVQ3 picture: three row-major sample planes (luma + two
/// chroma) sized to the macroblock grid.
///
/// The picture is `mb_cols × mb_rows` macroblocks. The luma plane is
/// `(mb_cols · 16) × (mb_rows · 16)` samples; each chroma plane is
/// `(mb_cols · 8) × (mb_rows · 8)` samples (the `4:2:0` 2× subsample of
/// the wiki §"Macroblock layer" — each macroblock owns a 16×16 luma
/// region and an 8×8 region per chroma plane).
///
/// All planes start zeroed. The per-macroblock reconstruction units in
/// [`crate::svq3_recon`] fill them via [`Self::blit_luma`] /
/// [`Self::blit_chroma`] after [`Self::bind_luma_neighbours`] /
/// [`Self::bind_chroma_neighbours`] supply each macroblock's neighbour
/// context.
#[derive(Debug, Clone)]
pub struct Svq3Picture {
    /// Macroblock columns (picture width in macroblocks).
    mb_cols: usize,
    /// Macroblock rows (picture height in macroblocks).
    mb_rows: usize,
    /// Luma plane, row-major, `(mb_cols·16) × (mb_rows·16)`.
    luma: Vec<u8>,
    /// Cb chroma plane, row-major, `(mb_cols·8) × (mb_rows·8)`.
    cb: Vec<u8>,
    /// Cr chroma plane, row-major, `(mb_cols·8) × (mb_rows·8)`.
    cr: Vec<u8>,
}

impl Svq3Picture {
    /// Allocate a zeroed picture sized to a `mb_cols × mb_rows` macroblock
    /// grid.
    ///
    /// # Panics
    ///
    /// Panics if `mb_cols == 0` or `mb_rows == 0` (a picture must contain
    /// at least one macroblock).
    #[must_use]
    pub fn new(mb_cols: usize, mb_rows: usize) -> Self {
        assert!(mb_cols > 0, "svq3 picture must have at least one MB column");
        assert!(mb_rows > 0, "svq3 picture must have at least one MB row");
        let luma_w = mb_cols * MB_LUMA_DIM;
        let luma_h = mb_rows * MB_LUMA_DIM;
        let chroma_w = mb_cols * CHROMA_PLANE_DIM;
        let chroma_h = mb_rows * CHROMA_PLANE_DIM;
        Self {
            mb_cols,
            mb_rows,
            luma: vec![0u8; luma_w * luma_h],
            cb: vec![0u8; chroma_w * chroma_h],
            cr: vec![0u8; chroma_w * chroma_h],
        }
    }

    /// Macroblock columns.
    #[inline]
    #[must_use]
    pub const fn mb_cols(&self) -> usize {
        self.mb_cols
    }

    /// Macroblock rows.
    #[inline]
    #[must_use]
    pub const fn mb_rows(&self) -> usize {
        self.mb_rows
    }

    /// Luma plane width in samples (`mb_cols · 16`).
    #[inline]
    #[must_use]
    pub const fn luma_width(&self) -> usize {
        self.mb_cols * MB_LUMA_DIM
    }

    /// Luma plane height in samples (`mb_rows · 16`).
    #[inline]
    #[must_use]
    pub const fn luma_height(&self) -> usize {
        self.mb_rows * MB_LUMA_DIM
    }

    /// Chroma plane width in samples (`mb_cols · 8`).
    #[inline]
    #[must_use]
    pub const fn chroma_width(&self) -> usize {
        self.mb_cols * CHROMA_PLANE_DIM
    }

    /// Chroma plane height in samples (`mb_rows · 8`).
    #[inline]
    #[must_use]
    pub const fn chroma_height(&self) -> usize {
        self.mb_rows * CHROMA_PLANE_DIM
    }

    /// The reconstructed luma plane as a borrowed row-major slice.
    #[inline]
    #[must_use]
    pub fn luma(&self) -> &[u8] {
        &self.luma
    }

    /// The reconstructed Cb chroma plane as a borrowed row-major slice.
    #[inline]
    #[must_use]
    pub fn cb(&self) -> &[u8] {
        &self.cb
    }

    /// The reconstructed Cr chroma plane as a borrowed row-major slice.
    #[inline]
    #[must_use]
    pub fn cr(&self) -> &[u8] {
        &self.cr
    }

    /// Read the reconstructed luma sample at picture-absolute pixel
    /// `(x, y)`.
    ///
    /// # Panics
    ///
    /// Panics if `(x, y)` is outside the luma plane.
    #[inline]
    #[must_use]
    pub fn luma_sample(&self, x: usize, y: usize) -> u8 {
        self.luma[y * self.luma_width() + x]
    }

    /// Read the reconstructed Cb sample at picture-absolute pixel
    /// `(x, y)`.
    ///
    /// # Panics
    ///
    /// Panics if `(x, y)` is outside the chroma plane.
    #[inline]
    #[must_use]
    pub fn cb_sample(&self, x: usize, y: usize) -> u8 {
        self.cb[y * self.chroma_width() + x]
    }

    /// Read the reconstructed Cr sample at picture-absolute pixel
    /// `(x, y)`.
    ///
    /// # Panics
    ///
    /// Panics if `(x, y)` is outside the chroma plane.
    #[inline]
    #[must_use]
    pub fn cr_sample(&self, x: usize, y: usize) -> u8 {
        self.cr[y * self.chroma_width() + x]
    }

    /// Populate a [`LumaMacroblock`]'s out-of-MB neighbour rows/columns
    /// (`above`, `leftcol`, `corner`) + availability flags from the
    /// already-reconstructed picture pixels surrounding the macroblock at
    /// raster position `pos`.
    ///
    /// The macroblock at `pos` occupies the 16×16 luma region whose
    /// top-left pixel is `(pos.mb_x·16, pos.mb_y·16)`. Its neighbour
    /// context (per [`LumaMacroblock::neighbours_at`]) is:
    ///
    /// * `above[x]` = picture pixel `(origin_x + x, origin_y − 1)` for
    ///   `x ∈ 0..16` — the bottom row of the macroblock directly above.
    ///   Available iff `pos.top_available`.
    /// * `leftcol[y]` = picture pixel `(origin_x − 1, origin_y + y)` for
    ///   `y ∈ 0..16` — the right column of the macroblock directly to the
    ///   left. Available iff `pos.left_available`.
    /// * `corner` = picture pixel `(origin_x − 1, origin_y − 1)` — the
    ///   above-left diagonal sample, read by a top-left sub-block's
    ///   diagonal predictor only when both edges are available.
    ///
    /// Because macroblocks are decoded in raster order, every neighbour
    /// pixel this reads is already reconstructed in the canvas. After this
    /// call, `mb` carries the exact neighbour context the per-MB
    /// reconstruction loops in [`crate::svq3_recon`] consume; on return the
    /// macroblock's own 16×16 `samples` are left untouched (zeroed by the
    /// caller's [`LumaMacroblock::new`]).
    ///
    /// # Panics
    ///
    /// Panics if `pos` lies outside this picture's macroblock grid.
    pub fn bind_luma_neighbours(&self, pos: Svq3MacroblockPosition, mb: &mut LumaMacroblock) {
        assert!(
            (pos.mb_x as usize) < self.mb_cols && (pos.mb_y as usize) < self.mb_rows,
            "macroblock position out of picture grid"
        );
        let origin_x = pos.mb_x as usize * MB_LUMA_DIM;
        let origin_y = pos.mb_y as usize * MB_LUMA_DIM;
        let width = self.luma_width();

        mb.above_available = pos.top_available;
        mb.left_available = pos.left_available;

        if pos.top_available {
            let row_base = (origin_y - 1) * width + origin_x;
            for (x, a) in mb.above.iter_mut().enumerate() {
                *a = self.luma[row_base + x];
            }
        } else {
            mb.above = [0u8; MB_LUMA_DIM];
        }

        if pos.left_available {
            for (y, l) in mb.leftcol.iter_mut().enumerate() {
                *l = self.luma[(origin_y + y) * width + (origin_x - 1)];
            }
        } else {
            mb.leftcol = [0u8; MB_LUMA_DIM];
        }

        mb.corner = if pos.top_available && pos.left_available {
            self.luma[(origin_y - 1) * width + (origin_x - 1)]
        } else {
            0
        };
    }

    /// Populate a [`ChromaPlane`]'s out-of-MB neighbour row/column
    /// (`above`, `leftcol`) + availability flags from the
    /// already-reconstructed chroma pixels surrounding the macroblock at
    /// raster position `pos`.
    ///
    /// `plane` selects which chroma plane (Cb or Cr) the neighbour pixels
    /// are read from. The macroblock at `pos` owns the 8×8 chroma region
    /// whose top-left pixel is `(pos.mb_x·8, pos.mb_y·8)`. SVQ3 chroma
    /// prediction is DC-only (wiki §"Intra prediction": "8×8 chroma always
    /// uses DC prediction"), so only the 8-sample above row + left column
    /// are read — there is no corner / plane fit (mirrored by
    /// [`ChromaPlane`] carrying no `corner` field).
    ///
    /// # Panics
    ///
    /// Panics if `pos` lies outside this picture's macroblock grid.
    pub fn bind_chroma_neighbours(
        &self,
        pos: Svq3MacroblockPosition,
        which: ChromaSelect,
        plane: &mut ChromaPlane,
    ) {
        assert!(
            (pos.mb_x as usize) < self.mb_cols && (pos.mb_y as usize) < self.mb_rows,
            "macroblock position out of picture grid"
        );
        let src = match which {
            ChromaSelect::Cb => &self.cb,
            ChromaSelect::Cr => &self.cr,
        };
        let origin_x = pos.mb_x as usize * CHROMA_PLANE_DIM;
        let origin_y = pos.mb_y as usize * CHROMA_PLANE_DIM;
        let width = self.chroma_width();

        plane.above_available = pos.top_available;
        plane.left_available = pos.left_available;

        if pos.top_available {
            let row_base = (origin_y - 1) * width + origin_x;
            for (x, a) in plane.above.iter_mut().enumerate() {
                *a = src[row_base + x];
            }
        } else {
            plane.above = [0u8; CHROMA_PLANE_DIM];
        }

        if pos.left_available {
            for (y, l) in plane.leftcol.iter_mut().enumerate() {
                *l = src[(origin_y + y) * width + (origin_x - 1)];
            }
        } else {
            plane.leftcol = [0u8; CHROMA_PLANE_DIM];
        }
    }

    /// Copy a reconstructed [`LumaMacroblock`]'s 16×16 `samples` into the
    /// picture's luma plane at the macroblock's pixel origin
    /// (`pos.mb_x·16`, `pos.mb_y·16`).
    ///
    /// After this call the macroblock's pixels are visible to subsequent
    /// macroblocks' [`Self::bind_luma_neighbours`] reads.
    ///
    /// # Panics
    ///
    /// Panics if `pos` lies outside this picture's macroblock grid.
    pub fn blit_luma(&mut self, pos: Svq3MacroblockPosition, mb: &LumaMacroblock) {
        assert!(
            (pos.mb_x as usize) < self.mb_cols && (pos.mb_y as usize) < self.mb_rows,
            "macroblock position out of picture grid"
        );
        let origin_x = pos.mb_x as usize * MB_LUMA_DIM;
        let origin_y = pos.mb_y as usize * MB_LUMA_DIM;
        let width = self.luma_width();
        for y in 0..MB_LUMA_DIM {
            let dst_base = (origin_y + y) * width + origin_x;
            let src_base = y * MB_LUMA_DIM;
            self.luma[dst_base..dst_base + MB_LUMA_DIM]
                .copy_from_slice(&mb.samples[src_base..src_base + MB_LUMA_DIM]);
        }
    }

    /// Copy a reconstructed [`ChromaPlane`]'s 8×8 `samples` into the
    /// selected chroma plane at the macroblock's pixel origin
    /// (`pos.mb_x·8`, `pos.mb_y·8`).
    ///
    /// # Panics
    ///
    /// Panics if `pos` lies outside this picture's macroblock grid.
    pub fn blit_chroma(
        &mut self,
        pos: Svq3MacroblockPosition,
        which: ChromaSelect,
        plane: &ChromaPlane,
    ) {
        assert!(
            (pos.mb_x as usize) < self.mb_cols && (pos.mb_y as usize) < self.mb_rows,
            "macroblock position out of picture grid"
        );
        let width = self.chroma_width();
        let dst = match which {
            ChromaSelect::Cb => &mut self.cb,
            ChromaSelect::Cr => &mut self.cr,
        };
        let origin_x = pos.mb_x as usize * CHROMA_PLANE_DIM;
        let origin_y = pos.mb_y as usize * CHROMA_PLANE_DIM;
        for y in 0..CHROMA_PLANE_DIM {
            let dst_base = (origin_y + y) * width + origin_x;
            let src_base = y * CHROMA_PLANE_DIM;
            dst[dst_base..dst_base + CHROMA_PLANE_DIM]
                .copy_from_slice(&plane.samples[src_base..src_base + CHROMA_PLANE_DIM]);
        }
    }
}

/// Selects which chroma plane (Cb or Cr) a [`Svq3Picture`] neighbour-bind
/// or blit operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaSelect {
    /// The Cb (blue-difference) chroma plane.
    Cb,
    /// The Cr (red-difference) chroma plane.
    Cr,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svq3::macroblock_position;

    #[test]
    fn dimensions_follow_mb_grid() {
        let pic = Svq3Picture::new(3, 2);
        assert_eq!(pic.mb_cols(), 3);
        assert_eq!(pic.mb_rows(), 2);
        assert_eq!(pic.luma_width(), 48);
        assert_eq!(pic.luma_height(), 32);
        assert_eq!(pic.chroma_width(), 24);
        assert_eq!(pic.chroma_height(), 16);
        assert_eq!(pic.luma().len(), 48 * 32);
        assert_eq!(pic.cb().len(), 24 * 16);
        assert_eq!(pic.cr().len(), 24 * 16);
    }

    #[test]
    fn fresh_picture_is_zeroed() {
        let pic = Svq3Picture::new(2, 2);
        assert!(pic.luma().iter().all(|&p| p == 0));
        assert!(pic.cb().iter().all(|&p| p == 0));
        assert!(pic.cr().iter().all(|&p| p == 0));
    }

    #[test]
    fn blit_then_sample_round_trips_luma() {
        let mut pic = Svq3Picture::new(2, 2);
        let mut mb = LumaMacroblock::new();
        // Fill the MB with a known per-pixel ramp.
        for y in 0..MB_LUMA_DIM {
            for x in 0..MB_LUMA_DIM {
                mb.samples[y * MB_LUMA_DIM + x] = ((x + y * 2) & 0xff) as u8;
            }
        }
        // Place at MB (1, 1) — bottom-right of a 2×2 grid.
        let pos = macroblock_position(3, 2).unwrap(); // index 3 = (1,1)
        assert_eq!((pos.mb_x, pos.mb_y), (1, 1));
        pic.blit_luma(pos, &mb);
        // Picture-absolute origin is (16, 16).
        for y in 0..MB_LUMA_DIM {
            for x in 0..MB_LUMA_DIM {
                assert_eq!(pic.luma_sample(16 + x, 16 + y), ((x + y * 2) & 0xff) as u8);
            }
        }
        // Top-left MB region remains zeroed.
        assert_eq!(pic.luma_sample(0, 0), 0);
    }

    #[test]
    fn blit_then_sample_round_trips_chroma() {
        let mut pic = Svq3Picture::new(2, 1);
        let mut plane = ChromaPlane::new();
        for y in 0..CHROMA_PLANE_DIM {
            for x in 0..CHROMA_PLANE_DIM {
                plane.samples[y * CHROMA_PLANE_DIM + x] = (10 + x + y) as u8;
            }
        }
        let pos = macroblock_position(1, 2).unwrap(); // index 1 = (1,0)
        pic.blit_chroma(pos, ChromaSelect::Cr, &plane);
        // Cr origin is (8, 0).
        for y in 0..CHROMA_PLANE_DIM {
            for x in 0..CHROMA_PLANE_DIM {
                assert_eq!(pic.cr_sample(8 + x, y), (10 + x + y) as u8);
            }
        }
        // Cb untouched.
        assert!(pic.cb().iter().all(|&p| p == 0));
    }

    #[test]
    fn top_left_mb_has_no_neighbours() {
        let pic = Svq3Picture::new(2, 2);
        let pos = macroblock_position(0, 2).unwrap(); // (0,0)
        let mut mb = LumaMacroblock::new();
        pic.bind_luma_neighbours(pos, &mut mb);
        assert!(!mb.above_available);
        assert!(!mb.left_available);
        assert_eq!(mb.corner, 0);
    }

    #[test]
    fn left_neighbour_binds_right_column_of_left_mb() {
        let mut pic = Svq3Picture::new(2, 1);
        // Reconstruct MB (0,0) with a ramp, blit it in.
        let mut left = LumaMacroblock::new();
        for y in 0..MB_LUMA_DIM {
            for x in 0..MB_LUMA_DIM {
                left.samples[y * MB_LUMA_DIM + x] = (x + y) as u8;
            }
        }
        let pos0 = macroblock_position(0, 2).unwrap();
        pic.blit_luma(pos0, &left);

        // Bind neighbours for MB (1,0): its leftcol is MB0's right column.
        let pos1 = macroblock_position(1, 2).unwrap();
        let mut right = LumaMacroblock::new();
        pic.bind_luma_neighbours(pos1, &mut right);
        assert!(right.left_available);
        assert!(!right.above_available);
        // MB0 right column = samples at x=15: value (15 + y).
        for y in 0..MB_LUMA_DIM {
            assert_eq!(right.leftcol[y], (15 + y) as u8);
        }
    }

    #[test]
    fn above_and_corner_bind_from_top_and_diagonal_mbs() {
        let mut pic = Svq3Picture::new(2, 2);
        // Fill the whole top MB row with a recognisable pattern.
        let mut top_left = LumaMacroblock::new();
        let mut top_right = LumaMacroblock::new();
        for y in 0..MB_LUMA_DIM {
            for x in 0..MB_LUMA_DIM {
                top_left.samples[y * MB_LUMA_DIM + x] = (100 + x) as u8;
                top_right.samples[y * MB_LUMA_DIM + x] = (200 + x) as u8;
            }
        }
        pic.blit_luma(macroblock_position(0, 2).unwrap(), &top_left);
        pic.blit_luma(macroblock_position(1, 2).unwrap(), &top_right);

        // MB (1,1): above = bottom row of top_right; corner = bottom-right
        // pixel of top_left (diagonal above-left).
        let pos = macroblock_position(3, 2).unwrap();
        let mut mb = LumaMacroblock::new();
        pic.bind_luma_neighbours(pos, &mut mb);
        assert!(mb.above_available);
        assert!(mb.left_available);
        // above[x] = top_right bottom row value (200 + x).
        for x in 0..MB_LUMA_DIM {
            assert_eq!(mb.above[x], (200 + x) as u8);
        }
        // corner = top_left pixel at (15, 15) = 100 + 15 = 115.
        assert_eq!(mb.corner, 115);
    }

    #[test]
    fn chroma_neighbours_select_correct_plane() {
        let mut pic = Svq3Picture::new(2, 1);
        let mut left = ChromaPlane::new();
        for y in 0..CHROMA_PLANE_DIM {
            for x in 0..CHROMA_PLANE_DIM {
                left.samples[y * CHROMA_PLANE_DIM + x] = (50 + x + y) as u8;
            }
        }
        pic.blit_chroma(macroblock_position(0, 2).unwrap(), ChromaSelect::Cb, &left);

        let pos1 = macroblock_position(1, 2).unwrap();
        let mut right = ChromaPlane::new();
        pic.bind_chroma_neighbours(pos1, ChromaSelect::Cb, &mut right);
        assert!(right.left_available);
        // Cb left column = MB0 right column (x=7): value 50 + 7 + y.
        for y in 0..CHROMA_PLANE_DIM {
            assert_eq!(right.leftcol[y], (50 + 7 + y) as u8);
        }

        // Cr was never written, so binding from Cr gives zeroed neighbours
        // even though left_available is set (the geometry is plane-agnostic;
        // the *pixels* come from the selected plane).
        let mut right_cr = ChromaPlane::new();
        pic.bind_chroma_neighbours(pos1, ChromaSelect::Cr, &mut right_cr);
        assert!(right_cr.left_available);
        assert!(right_cr.leftcol.iter().all(|&p| p == 0));
    }
}
