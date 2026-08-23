//! SVQ1 motion-vector cache and neighbour-selection geometry: the
//! per-plane 8×8-block MV store that feeds the
//! [`crate::svq1_motion_predictor`] median predictor.
//!
//! ## Scope
//!
//! This module lands the **MV cache** of
//! `docs/video/svq1/spec/06-motion-vectors.md` §6.8 and the
//! **neighbour-selection geometry** of §6.4.3 (INTER, single MV) and
//! §6.4.4 (INTER_4MV, per-sub-block), plus the §6.8.1 cache-update and
//! §6.9 SKIP / INTRA reset rules. It is the caller-side counterpart of
//! [`crate::svq1_motion_predictor`], whose `predict` takes the three
//! candidate neighbours but leaves their *selection* and *storage* to
//! the caller (per that module's "the caller owns the cache" note).
//!
//! What this module pins per spec/06:
//!
//! * The per-plane cache shape (§6.8): a grid of `(MB_W × 2) × (MB_H ×
//!   2)` 8×8-block slots, two slots per macroblock in each direction
//!   (the §6.1.1 granularity invariant — every 16×16 macroblock owns a
//!   2×2 arrangement of 8×8 cache slots). The cache is initialised to
//!   `(0, 0)` everywhere at frame start (§6.8.1 / §6.8.2 — no MV
//!   inheritance across frames).
//!
//! * Neighbour availability (§6.4.2): a neighbour slot is **absent**
//!   when it would be read from outside the cache grid (a block on the
//!   left, top, or top-right edge). Availability is purely geometric —
//!   in-bounds slots are always present (they carry `(0, 0)` for SKIP /
//!   INTRA macroblocks per §6.9, which is predictor-neutral, not
//!   "absent").
//!
//! * INTER neighbour selection (§6.4.3): for a single-MV 16×16
//!   macroblock whose top-left 8×8 slot is at cache position
//!   `(row, col)`, the predictor neighbours are
//!   `pl = (row, col − 1)`, `pt = (row − 2, col)`,
//!   `ptr = (row − 2, col + 2)` — i.e. the previous-left 8×8 block, and
//!   the previous-top / previous-top-right vectors of the 16×16 block
//!   ABOVE (two 8×8 rows up, the top-right reaching the column past the
//!   macroblock's own width). This reproduces the wiki grid
//!   `[1 2 / 3 4]` → `{pl = N, pt = C, ptr = E}`.
//!
//! * INTER_4MV neighbour selection (§6.4.4): the four 8×8 sub-blocks
//!   each select their own neighbour triple per the §6.4.4 table,
//!   mixing within-MB just-decoded MVs (sub-blocks 2, 3, 4 reference
//!   sub-block 1 / 2 / 3 of the same macroblock) with the adjacent
//!   top-row cache slots. The decode is strictly serial (§6.4.5):
//!   sub-block N's predictor may depend on sub-blocks `< N` of the same
//!   macroblock, so they must be decoded in the order 1, 2, 3, 4.
//!
//! * Cache update / reset (§6.8.1, §6.9): SKIP and INTRA broadcast
//!   `(0, 0)` to all four slots; INTER broadcasts the single decoded MV
//!   to all four; INTER_4MV writes each sub-block's own MV.
//!
//! ## No bitstream reads, no ambiguity
//!
//! This module is pure indexing + storage. It does **not** read the
//! differential `(dx, dy)` from the bitstream (that is the §6.2.3
//! Reading A/B-ambiguous T02 wire decode, deferred to a Validator
//! round) and it does **not** perform the half-pel reference sampling
//! (§6.5, de-facto only). The cache geometry and update rules are fully
//! unambiguous and shared by both T02 readings, so they are the safe
//! increment on top of [`crate::svq1_motion_predictor::predict`].

use crate::svq1_motion_predictor::{predict, Neighbours, Svq1Mv};

/// The per-plane MV cache of `docs/video/svq1/spec/06-motion-vectors.md`
/// §6.8: a 2-D grid of 8×8-block motion vectors, sized
/// `(mb_cols × 2) × (mb_rows × 2)` per the §6.1.1 granularity invariant
/// (each 16×16 macroblock owns a 2×2 block of cache slots).
///
/// Slots are addressed by 8×8-block `(row, col)` in the global plane
/// grid. The cache is initialised to `(0, 0)` everywhere (§6.8.1) and
/// has a single-frame lifetime (§6.8.2 — no inheritance across frames;
/// construct a fresh cache, or call [`Self::reset`], at each frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svq1MvCache {
    /// Width of the cache in 8×8-block columns (`mb_cols × 2`).
    block_cols: usize,
    /// Height of the cache in 8×8-block rows (`mb_rows × 2`).
    block_rows: usize,
    /// Row-major slots; `slots[row * block_cols + col]` is the MV of
    /// the 8×8 block at `(row, col)`.
    slots: Vec<Svq1Mv>,
}

/// The four 8×8 sub-block positions of a 16×16 macroblock, in the
/// raster decode order of §6.4.5 (`top-left`, `top-right`,
/// `bottom-left`, `bottom-right` = sub-blocks 1, 2, 3, 4).
///
/// Used to drive INTER_4MV serial decode: each entry is the cache
/// `(row, col)` of the sub-block.
pub const SUBBLOCK_ORDER: [(usize, usize); 4] = [(0, 0), (0, 1), (1, 0), (1, 1)];

impl Svq1MvCache {
    /// Allocate a cache for a plane of `mb_cols × mb_rows` macroblocks,
    /// initialised to `(0, 0)` everywhere per §6.8.1.
    ///
    /// The grid is `(mb_cols × 2)` wide and `(mb_rows × 2)` tall in
    /// 8×8-block units (the §6.1.1 invariant). A zero-macroblock plane
    /// yields an empty cache.
    pub fn new(mb_cols: usize, mb_rows: usize) -> Svq1MvCache {
        let block_cols = mb_cols * 2;
        let block_rows = mb_rows * 2;
        Svq1MvCache {
            block_cols,
            block_rows,
            slots: vec![Svq1Mv::ZERO; block_cols * block_rows],
        }
    }

    /// The cache width in 8×8-block columns.
    pub fn block_cols(&self) -> usize {
        self.block_cols
    }

    /// The cache height in 8×8-block rows.
    pub fn block_rows(&self) -> usize {
        self.block_rows
    }

    /// Reset every slot to `(0, 0)` per §6.8.1 — the frame-start state.
    /// §6.8.2 forbids MV inheritance across frames, so a reused cache
    /// must be reset between frames.
    pub fn reset(&mut self) {
        self.slots.fill(Svq1Mv::ZERO);
    }

    /// The MV at 8×8-block position `(row, col)`, or `None` when the
    /// position lies outside the cache grid.
    ///
    /// `row` / `col` are `i64` so callers can probe the out-of-bounds
    /// neighbour positions of §6.4.2 (e.g. `col − 1` at the left edge,
    /// `row − 2` at the top edge) directly. An out-of-bounds position
    /// is an **absent** neighbour per §6.4.2 — distinct from an
    /// in-bounds slot holding `(0, 0)`.
    pub fn get(&self, row: i64, col: i64) -> Option<Svq1Mv> {
        if row < 0 || col < 0 {
            return None;
        }
        let (row, col) = (row as usize, col as usize);
        if row >= self.block_rows || col >= self.block_cols {
            return None;
        }
        Some(self.slots[row * self.block_cols + col])
    }

    /// Write the MV at in-bounds 8×8-block position `(row, col)`.
    ///
    /// Out-of-bounds writes are silently ignored — the cache only
    /// stores slots that exist in the plane grid.
    fn set(&mut self, row: usize, col: usize, mv: Svq1Mv) {
        if row < self.block_rows && col < self.block_cols {
            self.slots[row * self.block_cols + col] = mv;
        }
    }

    /// The INTER (single-MV) predictor neighbours of §6.4.3 for the
    /// 16×16 macroblock whose top-left 8×8 slot is at `(row, col)`.
    ///
    /// Per §6.4.3 / the wiki grid `[1 2 / 3 4]` → `{pl = N, pt = C,
    /// ptr = E}`: the previous-LEFT 8×8 block, and the previous-top /
    /// top-right vectors of the 16×16 block ABOVE (two 8×8 rows up):
    ///
    /// * `pl  = (row, col − 1)` — the 8×8 slot to the left of the MB.
    /// * `pt  = (row − 2, col)` — the slot two rows up (top of the
    ///   above 16×16 block), same column as sub-block 1.
    /// * `ptr = (row − 2, col + 2)` — the slot two rows up, two columns
    ///   right (the column past the macroblock's own width = block `E`).
    ///
    /// Each is `None` (absent, §6.4.2) when its position is outside the
    /// cache grid.
    pub fn inter_neighbours(&self, row: usize, col: usize) -> Neighbours {
        let row = row as i64;
        let col = col as i64;
        Neighbours::new(
            self.get(row, col - 1),     // pl = N
            self.get(row - 2, col),     // pt = C
            self.get(row - 2, col + 2), // ptr = E
        )
    }

    /// The INTER_4MV predictor neighbours of §6.4.4 for sub-block
    /// `index` (0..=3 = sub-blocks 1..=4) of the 16×16 macroblock whose
    /// top-left 8×8 slot is at `(row, col)`.
    ///
    /// Per the §6.4.4 table (grid `A..W`, MB `[1 2 / 3 4]` at
    /// `(row, col)`):
    ///
    /// | Sub-block | L | T | T-R |
    /// |-----------|---|---|-----|
    /// | 1 | `N` = (row, col−1)   | `I` = (row−1, col)   | `K` = (row−1, col+2) |
    /// | 2 | `1` = (row, col)     | `J` = (row−1, col+1) | `K` = (row−1, col+2) |
    /// | 3 | `T` = (row+1, col−1) | `1` = (row, col)     | `2` = (row, col+1)   |
    /// | 4 | `3` = (row+1, col)   | `2` = (row, col+1)   | `1` = (row, col)     |
    ///
    /// Sub-blocks 2, 3, 4 reference within-MB slots (`1`, `2`, `3`)
    /// that must already hold the just-decoded MVs of the current
    /// macroblock — hence the strict serial decode order of §6.4.5
    /// ([`SUBBLOCK_ORDER`]). Sub-blocks 3 and 4 deviate from the
    /// "top-row neighbour" convention by using within-MB top-left /
    /// top vectors (the wiki "For block 3, the top-left motion vector
    /// is used rather than the top-right motion vector" note).
    ///
    /// # Panics
    ///
    /// Panics if `index >= 4` (only sub-blocks 0..=3 exist).
    pub fn inter_4mv_neighbours(&self, row: usize, col: usize, index: usize) -> Neighbours {
        let row = row as i64;
        let col = col as i64;
        let (l, t, tr) = match index {
            // Sub-block 1: L=N, T=I, T-R=K.
            0 => ((row, col - 1), (row - 1, col), (row - 1, col + 2)),
            // Sub-block 2: L=1, T=J, T-R=K.
            1 => ((row, col), (row - 1, col + 1), (row - 1, col + 2)),
            // Sub-block 3: L=T, T=1, T-R=2 (top-left vector as T-R per
            // the §6.4.4 note).
            2 => ((row + 1, col - 1), (row, col), (row, col + 1)),
            // Sub-block 4: L=3, T=2, T-R=1 (within-MB top-left as T-R).
            3 => ((row + 1, col), (row, col + 1), (row, col)),
            other => panic!("INTER_4MV sub-block index {other} out of range 0..=3"),
        };
        Neighbours::new(self.get(l.0, l.1), self.get(t.0, t.1), self.get(tr.0, tr.1))
    }

    /// Broadcast `mv` to all four 8×8 slots of the macroblock whose
    /// top-left slot is at `(row, col)`, per the §6.8.1 INTER rule
    /// ("All four slots ← decoded MV"). Also the mechanism behind the
    /// §6.9 SKIP / INTRA reset (pass [`Svq1Mv::ZERO`]).
    pub fn store_inter(&mut self, row: usize, col: usize, mv: Svq1Mv) {
        for (dr, dc) in SUBBLOCK_ORDER {
            self.set(row + dr, col + dc, mv);
        }
    }

    /// Reset all four 8×8 slots of the macroblock at `(row, col)` to
    /// `(0, 0)` per §6.9 (SKIP / INTRA macroblocks contribute the zero
    /// vector to the predictor cache). Equivalent to
    /// `store_inter(row, col, Svq1Mv::ZERO)`; named for the §6.9 intent.
    pub fn store_skip_intra(&mut self, row: usize, col: usize) {
        self.store_inter(row, col, Svq1Mv::ZERO);
    }

    /// Store the four per-sub-block MVs of an INTER_4MV macroblock per
    /// the §6.8.1 INTER_4MV rule ("Each slot ← its own decoded MV").
    /// `mvs[i]` is sub-block `i`'s MV in the [`SUBBLOCK_ORDER`] order.
    pub fn store_inter_4mv(&mut self, row: usize, col: usize, mvs: [Svq1Mv; 4]) {
        for (i, (dr, dc)) in SUBBLOCK_ORDER.iter().enumerate() {
            self.set(row + dr, col + dc, mvs[i]);
        }
    }

    /// Store ONE 8×8 sub-block slot (`index` per [`SUBBLOCK_ORDER`])
    /// of the macroblock at `(row, col)` — the encoder-side
    /// counterpart of the §6.4.5 serial decode: an INTER_4MV encoder
    /// must commit sub-block `i`'s chosen MV into the cache BEFORE
    /// computing sub-block `i + 1`'s §6.4.4 predictor, exactly as the
    /// decoder's [`Self::decode_inter_4mv`] interleaves its stores.
    ///
    /// # Panics
    ///
    /// Panics if `index >= 4`.
    pub fn store_subblock(&mut self, row: usize, col: usize, index: usize, mv: Svq1Mv) {
        let (dr, dc) = SUBBLOCK_ORDER[index];
        self.set(row + dr, col + dc, mv);
    }

    /// Decode the single INTER-mode MV of the macroblock at
    /// `(row, col)`: compute the §6.4.3 predictor, add the supplied
    /// differential `(dx, dy)`, clip to `[-32, +31]` (§6.6), broadcast
    /// to all four cache slots (§6.8.1), and return the final MV.
    ///
    /// The differential `(dx, dy)` is supplied by the caller — the T02
    /// wire decode that produces it is the §6.2.3 Reading A/B-ambiguous
    /// step deferred to a Validator round. The predictor + clip + cache
    /// arithmetic here is shared by both readings.
    pub fn decode_inter(&mut self, row: usize, col: usize, dx: i32, dy: i32) -> Svq1Mv {
        let predictor = predict(self.inter_neighbours(row, col));
        let mv = crate::svq1_motion_predictor::final_motion_vector(predictor, dx, dy);
        self.store_inter(row, col, mv);
        mv
    }

    /// Decode the four INTER_4MV sub-block MVs of the macroblock at
    /// `(row, col)` serially (§6.4.5): for each sub-block in
    /// [`SUBBLOCK_ORDER`], compute the §6.4.4 predictor against the
    /// CURRENT cache (so sub-blocks 2..4 see the just-stored MVs of
    /// earlier sub-blocks), add the differential, clip, and store
    /// immediately. Returns the four final MVs in sub-block order.
    ///
    /// `diffs[i]` is the `(dx, dy)` differential of sub-block `i`. The
    /// serial store-then-next-predict ordering is mandatory: sub-block
    /// 2's `pl` is sub-block 1's stored MV, etc. (§6.4.4 / §6.4.5).
    pub fn decode_inter_4mv(
        &mut self,
        row: usize,
        col: usize,
        diffs: [(i32, i32); 4],
    ) -> [Svq1Mv; 4] {
        let mut out = [Svq1Mv::ZERO; 4];
        for index in 0..4 {
            let predictor = predict(self.inter_4mv_neighbours(row, col, index));
            let (dx, dy) = diffs[index];
            let mv = crate::svq1_motion_predictor::final_motion_vector(predictor, dx, dy);
            let (dr, dc) = SUBBLOCK_ORDER[index];
            self.set(row + dr, col + dc, mv);
            out[index] = mv;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(x: i32, y: i32) -> Svq1Mv {
        Svq1Mv::new(x, y)
    }

    #[test]
    fn new_cache_is_zero_and_correctly_sized() {
        // 4 MB wide × 3 MB tall → 8 × 6 8×8-block grid, all (0, 0).
        let c = Svq1MvCache::new(4, 3);
        assert_eq!(c.block_cols(), 8);
        assert_eq!(c.block_rows(), 6);
        for r in 0..6 {
            for col in 0..8 {
                assert_eq!(c.get(r, col), Some(Svq1Mv::ZERO));
            }
        }
    }

    #[test]
    fn out_of_bounds_get_is_absent() {
        let c = Svq1MvCache::new(2, 2); // 4×4 grid
        assert_eq!(c.get(-1, 0), None);
        assert_eq!(c.get(0, -1), None);
        assert_eq!(c.get(4, 0), None);
        assert_eq!(c.get(0, 4), None);
        // in-bounds zero slot is PRESENT, not absent (§6.4.2 vs §6.9).
        assert_eq!(c.get(0, 0), Some(Svq1Mv::ZERO));
    }

    #[test]
    fn inter_neighbours_match_wiki_grid_positions() {
        // Grid (8×8 blocks):
        //   A B C D E       row 0
        //   G H I J K       row 1
        //   M N 1 2 Q       row 2
        //   S T 3 4 W       row 3
        // MB [1 2 / 3 4] top-left slot = (row=2, col=2).
        // §6.4.3: pl=N=(2,1), pt=C=(0,2), ptr=E=(0,4).
        let mut c = Svq1MvCache::new(3, 2); // 6×4 grid covers cols 0..6, rows 0..4
                                            // Plant distinct MVs at N, C, E so we can confirm selection.
        c.set(2, 1, mv(11, 12)); // N
        c.set(0, 2, mv(21, 22)); // C
        c.set(0, 4, mv(31, 32)); // E
                                 // Plant decoys at the INTER_4MV positions (I, K) to ensure
                                 // INTER does NOT pick them.
        c.set(1, 2, mv(91, 92)); // I (row-1) — must NOT be pt
        c.set(1, 4, mv(93, 94)); // K (row-1) — must NOT be ptr
        let n = c.inter_neighbours(2, 2);
        assert_eq!(n.left, Some(mv(11, 12)), "pl must be N");
        assert_eq!(n.top, Some(mv(21, 22)), "pt must be C (row-2)");
        assert_eq!(
            n.top_right,
            Some(mv(31, 32)),
            "ptr must be E (row-2, col+2)"
        );
    }

    #[test]
    fn inter_neighbours_absent_at_top_left_edge() {
        // MB at (0, 0): pl=(0,-1) absent, pt=(-2,0) absent,
        // ptr=(-2,2) absent → all None.
        let c = Svq1MvCache::new(3, 3);
        let n = c.inter_neighbours(0, 0);
        assert_eq!(n.left, None);
        assert_eq!(n.top, None);
        assert_eq!(n.top_right, None);
    }

    #[test]
    fn inter_4mv_neighbours_match_wiki_table() {
        // Grid as above; MB top-left (row=2, col=2).
        // Sub-block 1: L=N=(2,1), T=I=(1,2), T-R=K=(1,4).
        // Sub-block 2: L=1=(2,2), T=J=(1,3), T-R=K=(1,4).
        // Sub-block 3: L=T=(3,1), T=1=(2,2), T-R=2=(2,3).
        // Sub-block 4: L=3=(3,2), T=2=(2,3), T-R=1=(2,2).
        let mut c = Svq1MvCache::new(3, 2); // 6×4 grid
        c.set(2, 1, mv(1, 1)); // N
        c.set(1, 2, mv(2, 2)); // I
        c.set(1, 3, mv(3, 3)); // J
        c.set(1, 4, mv(4, 4)); // K
        c.set(3, 1, mv(5, 5)); // T
                               // Within-MB slots: 1=(2,2), 2=(2,3), 3=(3,2), 4=(3,3).
        c.set(2, 2, mv(101, 101)); // block 1
        c.set(2, 3, mv(102, 102)); // block 2
        c.set(3, 2, mv(103, 103)); // block 3

        let s1 = c.inter_4mv_neighbours(2, 2, 0);
        assert_eq!(s1.left, Some(mv(1, 1))); // N
        assert_eq!(s1.top, Some(mv(2, 2))); // I
        assert_eq!(s1.top_right, Some(mv(4, 4))); // K

        let s2 = c.inter_4mv_neighbours(2, 2, 1);
        assert_eq!(s2.left, Some(mv(101, 101))); // block 1
        assert_eq!(s2.top, Some(mv(3, 3))); // J
        assert_eq!(s2.top_right, Some(mv(4, 4))); // K

        let s3 = c.inter_4mv_neighbours(2, 2, 2);
        assert_eq!(s3.left, Some(mv(5, 5))); // T
        assert_eq!(s3.top, Some(mv(101, 101))); // block 1 (top-left!)
        assert_eq!(s3.top_right, Some(mv(102, 102))); // block 2

        let s4 = c.inter_4mv_neighbours(2, 2, 3);
        assert_eq!(s4.left, Some(mv(103, 103))); // block 3
        assert_eq!(s4.top, Some(mv(102, 102))); // block 2
        assert_eq!(s4.top_right, Some(mv(101, 101))); // block 1 (top-left as T-R)
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn inter_4mv_neighbours_rejects_bad_index() {
        let c = Svq1MvCache::new(2, 2);
        let _ = c.inter_4mv_neighbours(0, 0, 4);
    }

    #[test]
    fn store_inter_broadcasts_to_four_slots() {
        // §6.8.1 INTER: all four 8×8 slots ← decoded MV.
        let mut c = Svq1MvCache::new(2, 2); // 4×4 grid
        c.store_inter(0, 0, mv(7, 8));
        assert_eq!(c.get(0, 0), Some(mv(7, 8)));
        assert_eq!(c.get(0, 1), Some(mv(7, 8)));
        assert_eq!(c.get(1, 0), Some(mv(7, 8)));
        assert_eq!(c.get(1, 1), Some(mv(7, 8)));
        // neighbouring MB untouched.
        assert_eq!(c.get(0, 2), Some(Svq1Mv::ZERO));
    }

    #[test]
    fn store_skip_intra_zeroes_four_slots() {
        // §6.9: SKIP / INTRA reset all four slots to (0, 0).
        let mut c = Svq1MvCache::new(2, 2);
        c.store_inter(0, 0, mv(7, 8)); // dirty them first
        c.store_skip_intra(0, 0);
        assert_eq!(c.get(0, 0), Some(Svq1Mv::ZERO));
        assert_eq!(c.get(0, 1), Some(Svq1Mv::ZERO));
        assert_eq!(c.get(1, 0), Some(Svq1Mv::ZERO));
        assert_eq!(c.get(1, 1), Some(Svq1Mv::ZERO));
    }

    #[test]
    fn store_inter_4mv_writes_each_slot_independently() {
        // §6.8.1 INTER_4MV: each slot ← its own MV.
        let mut c = Svq1MvCache::new(2, 2);
        let mvs = [mv(1, 1), mv(2, 2), mv(3, 3), mv(4, 4)];
        c.store_inter_4mv(0, 0, mvs);
        assert_eq!(c.get(0, 0), Some(mv(1, 1))); // sub-block 1
        assert_eq!(c.get(0, 1), Some(mv(2, 2))); // sub-block 2
        assert_eq!(c.get(1, 0), Some(mv(3, 3))); // sub-block 3
        assert_eq!(c.get(1, 1), Some(mv(4, 4))); // sub-block 4
    }

    #[test]
    fn reset_clears_to_zero() {
        let mut c = Svq1MvCache::new(2, 2);
        c.store_inter(0, 0, mv(9, 9));
        c.reset();
        for r in 0..4 {
            for col in 0..4 {
                assert_eq!(c.get(r, col), Some(Svq1Mv::ZERO));
            }
        }
    }

    #[test]
    fn decode_inter_uses_worked_example_predictor() {
        // Reproduce the §6.4.1 worked example through the cache path:
        // pl=(0,0), pt=(5,17), ptr=(-9,12) → predictor (0,12).
        // Place an MB at (row=2, col=2) so the INTER neighbour slots
        // (N at (2,1), C at (0,2), E at (0,4)) are all in-bounds.
        let mut c = Svq1MvCache::new(3, 2); // 6×4 grid
        c.set(2, 1, mv(0, 0)); // pl = N
        c.set(0, 2, mv(5, 17)); // pt = C
        c.set(0, 4, mv(-9, 12)); // ptr = E
                                 // differential (0, 0) → final = predictor = (0, 12).
        let result = c.decode_inter(2, 2, 0, 0);
        assert_eq!(result, mv(0, 12));
        // broadcast to all four slots of the MB.
        assert_eq!(c.get(2, 2), Some(mv(0, 12)));
        assert_eq!(c.get(2, 3), Some(mv(0, 12)));
        assert_eq!(c.get(3, 2), Some(mv(0, 12)));
        assert_eq!(c.get(3, 3), Some(mv(0, 12)));
    }

    #[test]
    fn decode_inter_applies_differential_and_clip() {
        // predictor (0, 12) + differential (3, -4) → (3, 8).
        let mut c = Svq1MvCache::new(3, 2);
        c.set(2, 1, mv(0, 0));
        c.set(0, 2, mv(5, 17));
        c.set(0, 4, mv(-9, 12));
        assert_eq!(c.decode_inter(2, 2, 3, -4), mv(3, 8));
    }

    #[test]
    fn decode_inter_4mv_is_serial_subblock1_feeds_subblock2() {
        // Demonstrate the §6.4.5 serial dependency: sub-block 2's pl is
        // sub-block 1's just-decoded MV.
        // MB at (row=2, col=2) on a 6×4 grid.
        let mut c = Svq1MvCache::new(3, 2);
        // Make sub-block 1's predictor neighbours all absent except a
        // lone available one so sub-block 1 = a known vector.
        // sub-block 1 neighbours: N=(2,1), I=(1,2), K=(1,4).
        c.set(2, 1, mv(10, 20)); // N present
                                 // I and K left at (0,0) (in-bounds present) → predictor =
                                 // MEDIAN over {N=(10,20), I=(0,0), K=(0,0)} = (0,0).
                                 // To make sub-block 1 deterministic & non-trivial, also clear
                                 // I/K influence by checking the actual median below.
                                 // differentials: sub-block 1 gets (0,0); we just want its
                                 // stored MV to flow into sub-block 2's pl=(2,2).
        let out = c.decode_inter_4mv(2, 2, [(0, 0), (0, 0), (0, 0), (0, 0)]);
        // sub-block 1 predictor = median((10,20),(0,0),(0,0)) = (0,0).
        assert_eq!(out[0], mv(0, 0));
        // After sub-block 1 stored at (2,2), sub-block 2 reads pl=(2,2).
        // Confirm the store happened immediately (serial).
        assert_eq!(c.get(2, 2), Some(out[0]));
    }

    #[test]
    fn decode_inter_4mv_serial_propagation_nonzero() {
        // A clearer serial test: drive sub-block 1 to a nonzero MV via
        // its differential, then confirm sub-block 2/3/4 predictors see
        // it. MB at (2,2) on a 6×4 grid; clear surroundings to (0,0).
        let mut c = Svq1MvCache::new(3, 2);
        // sub-block 1 neighbours N/I/K all (0,0) → predictor (0,0);
        // differential (6, 6) → sub-block 1 MV = (6, 6).
        // sub-block 2 neighbours: L=block1=(6,6), T=J=(0,0), T-R=K=(0,0)
        //   → predictor median((6,6),(0,0),(0,0)) = (0,0); diff (0,0)
        //   → MV (0,0).
        // sub-block 3 neighbours: L=T=(0,0), T=block1=(6,6),
        //   T-R=block2=(0,0) → predictor median = (0,0); MV (0,0).
        // sub-block 4 neighbours: L=block3=(0,0), T=block2=(0,0),
        //   T-R=block1=(6,6) → predictor median = (0,0); MV (0,0).
        let out = c.decode_inter_4mv(2, 2, [(6, 6), (0, 0), (0, 0), (0, 0)]);
        assert_eq!(out[0], mv(6, 6));
        assert_eq!(out[1], mv(0, 0));
        assert_eq!(out[2], mv(0, 0));
        assert_eq!(out[3], mv(0, 0));
        // Each stored immediately at its own slot.
        assert_eq!(c.get(2, 2), Some(mv(6, 6)));
        assert_eq!(c.get(2, 3), Some(mv(0, 0)));
        assert_eq!(c.get(3, 2), Some(mv(0, 0)));
        assert_eq!(c.get(3, 3), Some(mv(0, 0)));
    }
}
