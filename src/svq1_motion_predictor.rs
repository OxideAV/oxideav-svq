//! SVQ1 motion-vector predictor: the median-of-three baseline that
//! the per-block differential motion vector is added against.
//!
//! ## Scope
//!
//! This module lands the **median-of-three predictor** documented in
//! `docs/video/svq1/spec/06-motion-vectors.md` §6.4 — the
//! component-independent median of the previous-left, previous-top,
//! and previous-top-right neighbour motion vectors that produces the
//! per-block predictor baseline, plus the §6.4.2 absent-neighbour
//! fallback and the §6.6 final-vector component clip to `[-32, +31]`.
//!
//! What this module pins per spec/06:
//!
//! * The motion-vector value type — an `(x, y)` pair of signed
//!   half-pel components ([`Svq1Mv`]), per §6 the "MV" definition.
//! * The component-independent three-way median ([`median3`]): the
//!   predicted `x` is the median of the three candidate `x`
//!   components and the predicted `y` is the median of the three
//!   candidate `y` components, per §6.4.1. The median may equal none
//!   of the inputs (e.g. `MEDIAN(0, 5, -9) = 0`).
//! * The absent-neighbour fallback table of §6.4.2: when one or more
//!   of the three neighbours is unavailable (block on the left / top /
//!   top-right edge of the MV cache), the predictor is computed by
//!   the rules of §6.4.2's table — `(0, 0)` substituted into the
//!   median when two neighbours are present, the single available
//!   vector taken verbatim when exactly one is present (NOT a median
//!   against two `(0, 0)`s), and `(0, 0)` when none are present.
//! * The §6.6 final-vector clip: after the differential `(dx, dy)`
//!   is added to the predictor, each component is independently
//!   saturated to `[-32, +31]` ([`clip_component`] /
//!   [`final_motion_vector`]).
//!
//! What this module does **not** cover (and why):
//!
//! * Reading the differential `(dx, dy)` from the bitstream via the
//!   T02 component VLC. Per §6.2.3 / §6.10 open-item 1 the per-
//!   component wire protocol is one of two readings (Reading A — the
//!   wiki peek-bit / VLC-magnitude / sign-bit algorithm; Reading B —
//!   a direct signed VLC), and the disambiguation is flagged as a
//!   Validator-round task. Both readings are bit-stream-affecting, so
//!   wiring the differential decode now would risk pinning the wrong
//!   one. The predictor + clip arithmetic in this module is shared by
//!   BOTH readings and is fully unambiguous, so it is the safe
//!   increment.
//! * The half-pel reference sampling / interpolation (§6.5). Per
//!   §6.10 open-items 3 and 4 the exact interpolator rounding and
//!   the edge-of-frame behaviour are de-facto only and flagged for a
//!   Validator round; they are not pinned here.
//! * Maintaining the per-plane MV cache across a frame (§6.8). This
//!   module is pure per-block arithmetic: the caller owns the cache
//!   and supplies the three neighbour vectors (or marks them absent).
//!
//! ## No bitstream reads
//!
//! This module is pure arithmetic — it does not touch
//! [`crate::BitReader`]. The future round that resolves the Reading
//! A/B disambiguation and wires the T02 component VLC will read the
//! differential and call [`final_motion_vector`] with the decoded
//! `(dx, dy)` and the [`predict`] baseline.

/// The lower bound of an SVQ1 motion-vector component, `-32`, per
/// `docs/video/svq1/spec/06-motion-vectors.md` §6.6 (the final
/// vector is clipped to the closed range `[-32, +31]`).
pub const MV_COMPONENT_MIN: i32 = -32;

/// The upper bound of an SVQ1 motion-vector component, `+31`, per
/// `docs/video/svq1/spec/06-motion-vectors.md` §6.6.
pub const MV_COMPONENT_MAX: i32 = 31;

/// A SVQ1 motion vector: an `(x, y)` pair of signed half-pel
/// components per `docs/video/svq1/spec/06-motion-vectors.md` §6.
///
/// A component value of `1` denotes a displacement of one half-pixel
/// (per §6 "a component value of 1 means 'the reference sample at the
/// corresponding position plus one-half pixel'"). The components are
/// stored as `i32` so the intermediate `predictor + differential`
/// sum of §6.6 can overflow the `[-32, +31]` final range before the
/// clip is applied (per §6.6 / §6.2.3 Reading A, where the predictor
/// can produce values outside `[-32, +31]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Svq1Mv {
    /// The horizontal half-pel component.
    pub x: i32,
    /// The vertical half-pel component.
    pub y: i32,
}

impl Svq1Mv {
    /// The zero motion vector `(0, 0)` — the absent-neighbour and
    /// SKIP / INTRA cache value per
    /// `docs/video/svq1/spec/06-motion-vectors.md` §6.4.2 / §6.9.
    pub const ZERO: Svq1Mv = Svq1Mv { x: 0, y: 0 };

    /// Construct a motion vector from its two components.
    pub const fn new(x: i32, y: i32) -> Svq1Mv {
        Svq1Mv { x, y }
    }
}

/// The three-way median of `a`, `b`, and `c` per
/// `docs/video/svq1/spec/06-motion-vectors.md` §6.4.1 — the
/// middle value once the three are ordered.
///
/// Computed without branching on a full sort: the median equals
/// `max(min(a, b), min(max(a, b), c))`, which is the standard
/// branch-light median-of-three. The result may equal none of the
/// inputs only when there are ties; for distinct inputs it is one of
/// the three (e.g. `MEDIAN(0, 5, -9) = 0`, `MEDIAN(0, 17, 12) = 12`
/// per §6.4.1's worked example).
pub const fn median3(a: i32, b: i32, c: i32) -> i32 {
    let lo_ab = if a < b { a } else { b };
    let hi_ab = if a < b { b } else { a };
    // median = max(min(a, b), min(max(a, b), c))
    let min_hiab_c = if hi_ab < c { hi_ab } else { c };
    if lo_ab > min_hiab_c {
        lo_ab
    } else {
        min_hiab_c
    }
}

/// Clip a single motion-vector component to the `[-32, +31]` range
/// of `docs/video/svq1/spec/06-motion-vectors.md` §6.6. Applied
/// independently to each component of the FINAL motion vector (after
/// the predictor and differential are summed).
pub const fn clip_component(value: i32) -> i32 {
    if value < MV_COMPONENT_MIN {
        MV_COMPONENT_MIN
    } else if value > MV_COMPONENT_MAX {
        MV_COMPONENT_MAX
    } else {
        value
    }
}

/// The three candidate neighbour motion vectors that feed the
/// median-of-three predictor of
/// `docs/video/svq1/spec/06-motion-vectors.md` §6.4.
///
/// Each slot is `Some(mv)` when the neighbour 8×8 block lies inside
/// the current frame's MV cache, or `None` when the neighbour would
/// be read from outside the cache (the current block is on the left,
/// top, or top-right edge) per §6.4.2's "absent-neighbour" definition.
///
/// * `left` — the previous-left neighbour (`pl`).
/// * `top` — the previous-top neighbour (`pt`).
/// * `top_right` — the previous-top-right neighbour (`ptr`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Neighbours {
    /// The previous-left neighbour MV (`pl`), or `None` if absent.
    pub left: Option<Svq1Mv>,
    /// The previous-top neighbour MV (`pt`), or `None` if absent.
    pub top: Option<Svq1Mv>,
    /// The previous-top-right neighbour MV (`ptr`), or `None` if
    /// absent.
    pub top_right: Option<Svq1Mv>,
}

impl Neighbours {
    /// Construct the neighbour set from three optional vectors.
    pub const fn new(
        left: Option<Svq1Mv>,
        top: Option<Svq1Mv>,
        top_right: Option<Svq1Mv>,
    ) -> Neighbours {
        Neighbours {
            left,
            top,
            top_right,
        }
    }
}

/// Compute the per-block predictor motion vector from its three
/// neighbours per `docs/video/svq1/spec/06-motion-vectors.md` §6.4.
///
/// The predictor is computed by the §6.4.2 absent-neighbour fallback
/// table:
///
/// | Available neighbours        | Predictor                                  |
/// |-----------------------------|--------------------------------------------|
/// | all three (`pl`,`pt`,`ptr`) | component-wise `MEDIAN(pl, pt, ptr)`        |
/// | exactly two present         | `MEDIAN(present, present, (0,0))`           |
/// | exactly one present         | the single present vector verbatim         |
/// | none present                | `(0, 0)`                                    |
///
/// The "exactly one present" case is a short-circuit, NOT a median
/// against two injected `(0, 0)`s: per §6.4.2 / §6.10 open-item 7 the
/// wiki source's "In the case that only one vector is available, that
/// vector becomes the predicting vector" line is explicit that the
/// single available vector IS the predictor (a median against two
/// zeros would clamp every component toward zero, which the wiki
/// refutes). For two or three present vectors the median formula is
/// used with `(0, 0)` substituted for any single absent slot per the
/// "A vector of (0,0) is used in place of any unavailable vectors"
/// line.
pub fn predict(neighbours: Neighbours) -> Svq1Mv {
    match (neighbours.left, neighbours.top, neighbours.top_right) {
        // None present → (0, 0) trivially.
        (None, None, None) => Svq1Mv::ZERO,

        // Exactly one present → that vector verbatim (short-circuit,
        // §6.4.2 "that vector becomes the predicting vector").
        (Some(mv), None, None) | (None, Some(mv), None) | (None, None, Some(mv)) => mv,

        // Two or three present → component-wise median with (0, 0)
        // substituted for any single absent slot, §6.4.2.
        (pl, pt, ptr) => {
            let pl = pl.unwrap_or(Svq1Mv::ZERO);
            let pt = pt.unwrap_or(Svq1Mv::ZERO);
            let ptr = ptr.unwrap_or(Svq1Mv::ZERO);
            Svq1Mv {
                x: median3(pl.x, pt.x, ptr.x),
                y: median3(pl.y, pt.y, ptr.y),
            }
        }
    }
}

/// Combine a predictor and a decoded differential into the final
/// motion vector per `docs/video/svq1/spec/06-motion-vectors.md`
/// §6.6: `final = clip(predictor + differential, -32, +31)`, with
/// the clip applied independently to each component.
///
/// `dx` / `dy` are the per-component differentials the encoder
/// transmitted (the wire decode of which is a future round per §6.2.3
/// open-item 1); `predictor` is the [`predict`] baseline. The clip is
/// genuine saturation under §6.2.3 Reading A (the predictor can
/// exceed `[-32, +31]`) and a no-op under Reading B (the wire value
/// is already in range) — both readings agree on the per-component
/// clip per §6.6.
pub const fn final_motion_vector(predictor: Svq1Mv, dx: i32, dy: i32) -> Svq1Mv {
    Svq1Mv {
        x: clip_component(predictor.x + dx),
        y: clip_component(predictor.y + dy),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median3_worked_example_x_and_y() {
        // §6.4.1 worked example: pl=(0,0), pt=(5,17), ptr=(-9,12)
        // → predictor.x = MEDIAN(0, 5, -9) = 0,
        //   predictor.y = MEDIAN(0, 17, 12) = 12.
        assert_eq!(median3(0, 5, -9), 0);
        assert_eq!(median3(0, 17, 12), 12);
    }

    #[test]
    fn median3_is_order_independent() {
        // The middle value is independent of argument order.
        for &(a, b, c) in &[(0, 5, -9), (-9, 0, 5), (5, -9, 0), (5, 0, -9)] {
            assert_eq!(median3(a, b, c), 0);
        }
        for &(a, b, c) in &[(0, 17, 12), (12, 0, 17), (17, 12, 0)] {
            assert_eq!(median3(a, b, c), 12);
        }
    }

    #[test]
    fn median3_with_ties() {
        // Tie cases: the median equals the repeated value or the odd
        // one out as the ordering dictates.
        assert_eq!(median3(7, 7, 3), 7);
        assert_eq!(median3(7, 3, 7), 7);
        assert_eq!(median3(3, 7, 7), 7);
        assert_eq!(median3(4, 4, 4), 4);
    }

    #[test]
    fn predict_all_three_uses_median() {
        // §6.4.1 worked example as a full predict() call.
        let n = Neighbours::new(
            Some(Svq1Mv::new(0, 0)),
            Some(Svq1Mv::new(5, 17)),
            Some(Svq1Mv::new(-9, 12)),
        );
        assert_eq!(predict(n), Svq1Mv::new(0, 12));
    }

    #[test]
    fn predict_none_present_is_zero() {
        // §6.4.2: no neighbours → (0, 0).
        assert_eq!(predict(Neighbours::default()), Svq1Mv::ZERO);
    }

    #[test]
    fn predict_one_present_is_that_vector_verbatim() {
        // §6.4.2 "that vector becomes the predicting vector" — NOT a
        // median against two injected zeros (which would give (0, 0)
        // here, refuted by the wiki).
        let only_left = Neighbours::new(Some(Svq1Mv::new(5, 17)), None, None);
        assert_eq!(predict(only_left), Svq1Mv::new(5, 17));

        let only_top = Neighbours::new(None, Some(Svq1Mv::new(-9, 12)), None);
        assert_eq!(predict(only_top), Svq1Mv::new(-9, 12));

        let only_tr = Neighbours::new(None, None, Some(Svq1Mv::new(3, -4)));
        assert_eq!(predict(only_tr), Svq1Mv::new(3, -4));
    }

    #[test]
    fn predict_two_present_substitutes_zero_into_median() {
        // §6.4.2: two present → MEDIAN(present, present, (0, 0)).
        // left=(5, 17), top=(-9, 12), ptr absent → (0, 0) injected.
        // x = MEDIAN(5, -9, 0) = 0; y = MEDIAN(17, 12, 0) = 12.
        let n = Neighbours::new(Some(Svq1Mv::new(5, 17)), Some(Svq1Mv::new(-9, 12)), None);
        assert_eq!(predict(n), Svq1Mv::new(0, 12));

        // left=(8, 8), ptr=(2, 2), top absent.
        // x = MEDIAN(8, 0, 2) = 2; y = MEDIAN(8, 0, 2) = 2.
        let n2 = Neighbours::new(Some(Svq1Mv::new(8, 8)), None, Some(Svq1Mv::new(2, 2)));
        assert_eq!(predict(n2), Svq1Mv::new(2, 2));
    }

    #[test]
    fn one_present_differs_from_two_present_with_zero() {
        // Demonstrate the short-circuit is load-bearing: a lone
        // (5, 17) predicts (5, 17), but (5, 17) median'd with two
        // zeros would be (0, 0). The §6.4.2 one-present rule keeps
        // the vector intact.
        let lone = Neighbours::new(Some(Svq1Mv::new(5, 17)), None, None);
        assert_eq!(predict(lone), Svq1Mv::new(5, 17));
        // contrast: if the (wrong) median-against-two-zeros rule were
        // used, median3 would collapse it to zero.
        assert_eq!(median3(5, 0, 0), 0);
        assert_eq!(median3(17, 0, 0), 0);
    }

    #[test]
    fn clip_component_saturates_both_ends() {
        // §6.6 closed range [-32, +31].
        assert_eq!(clip_component(-33), MV_COMPONENT_MIN);
        assert_eq!(clip_component(-32), -32);
        assert_eq!(clip_component(0), 0);
        assert_eq!(clip_component(31), 31);
        assert_eq!(clip_component(32), MV_COMPONENT_MAX);
        assert_eq!(clip_component(1000), 31);
    }

    #[test]
    fn final_motion_vector_adds_then_clips() {
        // §6.4.1 / §6.6: predictor (0, 12), differential (dx, dy)
        // → final = clip(0 + dx, 12 + dy).
        let p = Svq1Mv::new(0, 12);
        assert_eq!(final_motion_vector(p, 0, 0), Svq1Mv::new(0, 12));
        assert_eq!(final_motion_vector(p, 3, -4), Svq1Mv::new(3, 8));

        // §6.6 saturation example: predictor.x = 25, dx = 12 → 37
        // clipped to 31.
        let p2 = Svq1Mv::new(25, 0);
        assert_eq!(final_motion_vector(p2, 12, 0), Svq1Mv::new(31, 0));

        // negative saturation.
        let p3 = Svq1Mv::new(-30, 0);
        assert_eq!(final_motion_vector(p3, -10, 0), Svq1Mv::new(-32, 0));
    }

    #[test]
    fn mv_zero_constant() {
        assert_eq!(Svq1Mv::ZERO, Svq1Mv::new(0, 0));
        assert_eq!(Svq1Mv::default(), Svq1Mv::ZERO);
    }
}
