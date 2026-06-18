//! SVQ3 intra-prediction pixel arithmetic (structural).
//!
//! ## Provenance
//!
//! Round 282 implements the one intra predictor the wiki spec pins
//! completely in `docs/video/svq3/wiki/Sorenson_Video_3.wiki`
//! §"Intra prediction" (verbatim local mirror of the multimedia.cx
//! `Sorenson_Video_3` wiki page). The section opens with "Intra
//! prediction is the same as in H.264 except for the following
//! quirks" and then spells the first quirk out in full:
//!
//! > 4x4 diagonal down prediction is performed as
//! >
//! > ```text
//! >   a b c c
//! >   b c c c
//! >   c c c c
//! >   c c c c
//! > ```
//! >
//! > where `a = (left[1] + top[1]) / 2`, `b = (left[2] + top[2]) / 2`
//! > and `c = (left[3] + top[3]) / 2`.
//!
//! Both halves of that quirk land here verbatim: the three per-sample
//! closed forms as the [`diagonal_down_sample`] `const fn` (one
//! neighbour-pair average with the spec's plain integer `/ 2` — no
//! rounding bias is present in the spec text, and the operands are
//! non-negative samples so the division is a floor), and the 4×4
//! fill picture as the [`DIAGONAL_DOWN_PATTERN`] placement table plus
//! the [`predict_diagonal_down_4x4`] block predictor that combines
//! the two.
//!
//! The spec formulas consume `left[1..=3]` and `top[1..=3]` only;
//! element `0` of either neighbour array is never referenced by this
//! predictor. The three consumed indices are surfaced as
//! [`DIAGONAL_DOWN_NEIGHBOUR_INDICES`] so callers assembling the
//! neighbour arrays can corroborate the layout.
//!
//! ## Open work
//!
//! * The numeric intra-prediction-mode value (`0..=4` in
//!   [`crate::svq3_mb::INTRA_PRED_TABLE`]) that selects this
//!   predictor is NOT pinned in `docs/video/svq3/` — the wiki names
//!   the predictor ("diagonal down") without binding it to a mode
//!   number. The dispatch table that routes a resolved mode to a
//!   predictor function is deferred until docs pin the binding.
//! * The remaining 4×4 intra predictors, the 16×16 predictors (the
//!   wiki pins only "plane prediction is the same as in H.264 but
//!   transposed"), and the chroma DC predictor ("8x8 chroma always
//!   uses DC prediction") are back-referenced to H.264 rather than
//!   spelled out locally — `docs/video/svq3/spec/01-reconstruction-composition.md`
//!   Gap 4 now carries their sample equations; their block predictors
//!   are deferred to a later round.
//!
//! ## Reconstruction-composition writeback (spec/01 Gap 5)
//!
//! `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 5 pins
//! the predicted+residual writeback as the standard 8-bit saturating
//! sum with **no extra rounding on the add** — all rounding already
//! lives inside the dequant/transform pass
//! ([`crate::svq3_dequant`]) and the interpolation filters:
//!
//! > ```text
//! > recon[x,y] = Clip1( pred[x,y] + residual[x,y] )
//! >            = clip( pred[x,y] + residual[x,y], 0, 255 )
//! > ```
//!
//! That writeback lands here as [`reconstruct_sample`] (one clamped
//! sum) and [`reconstruct_4x4`] (the 4×4-block composition that takes
//! a predicted `[u8; 16]` block — e.g. from
//! [`predict_diagonal_down_4x4`] — and the dequantised/transformed
//! residual `[i32; 16]` from [`crate::svq3_dequant`] and produces the
//! reconstructed `[u8; 16]`). The clamp is the ordinary H.264
//! `Clip1_Y` / `Clip1_C` at `BitDepth = 8` ⇒ range `[0, 255]`
//! ([`RECON_SAMPLE_MIN`] / [`RECON_SAMPLE_MAX`]).
//!
//! ## Open work
//!
//! `Svq3DecoderHandle::receive_frame` continues to return
//! `oxideav_core::Error::Unsupported` — this module lands the
//! per-block pixel arithmetic and writeback composition only; the
//! macroblock loop that drives predictor selection, residual decode,
//! and writeback is not yet assembled.

/// Width / height of the 4×4 intra-predicted sub-block, and the
/// length of the `left` / `top` neighbour arrays the spec formulas
/// index into.
pub const PRED_4X4_DIM: usize = 4;

/// Number of samples in one 4×4 predicted block
/// (`PRED_4X4_DIM * PRED_4X4_DIM`).
pub const PRED_4X4_SAMPLES: usize = PRED_4X4_DIM * PRED_4X4_DIM;

/// The neighbour-array indices the spec's three closed forms consume,
/// in `(a, b, c)` order: `a` averages `left[1]` / `top[1]`, `b`
/// averages `left[2]` / `top[2]`, `c` averages `left[3]` / `top[3]`.
///
/// Element `0` of either neighbour array is never referenced by the
/// diagonal-down predictor.
pub const DIAGONAL_DOWN_NEIGHBOUR_INDICES: [usize; 3] = [1, 2, 3];

/// The 4×4 fill picture from the wiki spec's §"Intra prediction",
/// flattened row-major (`DIAGONAL_DOWN_PATTERN[row * 4 + col]`):
///
/// ```text
///   a b c c
///   b c c c
///   c c c c
///   c c c c
/// ```
///
/// Each entry selects one of the three derived samples: `0` ⇒ `a`,
/// `1` ⇒ `b`, `2` ⇒ `c`.
#[rustfmt::skip]
pub const DIAGONAL_DOWN_PATTERN: [u8; PRED_4X4_SAMPLES] = [
    0, 1, 2, 2,
    1, 2, 2, 2,
    2, 2, 2, 2,
    2, 2, 2, 2,
];

/// One diagonal-down predicted sample: the wiki spec's
/// `(left[k] + top[k]) / 2` closed form applied to a single
/// neighbour pair.
///
/// The spec writes a plain integer `/ 2` with no rounding bias; both
/// operands are unsigned samples so the division is an exact floor.
/// The result always fits in `u8` (`(255 + 255) / 2 = 255`).
///
/// ```
/// use oxideav_svq::svq3_pred::diagonal_down_sample;
///
/// // c = (left[3] + top[3]) / 2 with left[3] = 30, top[3] = 31:
/// // floor(61 / 2) = 30.
/// assert_eq!(diagonal_down_sample(30, 31), 30);
/// ```
pub const fn diagonal_down_sample(left_k: u8, top_k: u8) -> u8 {
    ((left_k as u16 + top_k as u16) / 2) as u8
}

/// The 4×4 diagonal-down intra predictor from the wiki spec's
/// §"Intra prediction", combining the three
/// [`diagonal_down_sample`] closed forms (`a` / `b` / `c` from
/// neighbour indices `1` / `2` / `3`) with the
/// [`DIAGONAL_DOWN_PATTERN`] fill picture.
///
/// `left` and `top` are the previously-reconstructed neighbour
/// sample arrays the spec formulas index into; element `0` of either
/// array is never referenced. The return value is the predicted 4×4
/// block flattened row-major (`out[row * 4 + col]`), matching the
/// row-major block layout used by [`crate::svq3_scan`] and
/// [`crate::svq3_dequant`], so the eventual predicted+residual
/// writeback can combine the two element-wise.
///
/// ```
/// use oxideav_svq::svq3_pred::predict_diagonal_down_4x4;
///
/// let left = [9, 10, 20, 30];
/// let top = [7, 14, 21, 31];
/// // a = (10 + 14) / 2 = 12, b = (20 + 21) / 2 = 20,
/// // c = (30 + 31) / 2 = 30.
/// assert_eq!(
///     predict_diagonal_down_4x4(left, top),
///     [
///         12, 20, 30, 30, //
///         20, 30, 30, 30, //
///         30, 30, 30, 30, //
///         30, 30, 30, 30, //
///     ]
/// );
/// ```
pub const fn predict_diagonal_down_4x4(
    left: [u8; PRED_4X4_DIM],
    top: [u8; PRED_4X4_DIM],
) -> [u8; PRED_4X4_SAMPLES] {
    let derived = [
        diagonal_down_sample(left[1], top[1]),
        diagonal_down_sample(left[2], top[2]),
        diagonal_down_sample(left[3], top[3]),
    ];
    let mut out = [0u8; PRED_4X4_SAMPLES];
    let mut i = 0;
    while i < PRED_4X4_SAMPLES {
        out[i] = derived[DIAGONAL_DOWN_PATTERN[i] as usize];
        i += 1;
    }
    out
}

/// Minimum reconstructed sample value — the lower bound of the
/// spec/01 Gap 5 `Clip1` saturating clamp at `BitDepth = 8`.
///
/// Per `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 5
/// the writeback clamp is the ordinary H.264 `Clip1_Y` / `Clip1_C`
/// with `BitDepth = 8`, i.e. `clip(·, 0, 255)`.
pub const RECON_SAMPLE_MIN: i32 = 0;

/// Maximum reconstructed sample value — the upper bound of the
/// spec/01 Gap 5 `Clip1` saturating clamp at `BitDepth = 8`
/// (`(1 << 8) - 1 = 255`).
pub const RECON_SAMPLE_MAX: i32 = 255;

/// Compose one reconstructed sample from a predicted sample and its
/// residual, applying the spec/01 Gap 5 saturating clamp.
///
/// `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 5 pins
/// per-sample reconstruction as
///
/// ```text
///   recon[x,y] = Clip1( pred[x,y] + residual[x,y] )
///              = clip( pred[x,y] + residual[x,y], 0, 255 )
/// ```
///
/// with **no per-sample rounding term on the add itself** — all
/// rounding already lives inside the dequant/transform pass
/// ([`crate::svq3_dequant`], the fused `+0x80000 … >> 20`) and the
/// interpolation filters. This helper therefore performs the plain
/// signed sum `pred + residual` and clamps it into
/// `[RECON_SAMPLE_MIN, RECON_SAMPLE_MAX]` (`[0, 255]`).
///
/// `pred` is a previously-produced predicted sample (intra predictor
/// output such as [`predict_diagonal_down_4x4`], or an inter
/// motion-compensated predictor); `residual` is the inverse-
/// transformed, dequantised coefficient from [`crate::svq3_dequant`].
/// The sum is computed in `i64` before clamping so a residual that
/// drives the sum below `0` or above `255` — including a pathological
/// residual at the `i32` extremes — saturates rather than wrapping.
///
/// ```
/// use oxideav_svq::svq3_pred::reconstruct_sample;
///
/// // In-range sum passes through unchanged.
/// assert_eq!(reconstruct_sample(100, 27), 127);
/// // Negative residual that underflows saturates to 0.
/// assert_eq!(reconstruct_sample(10, -50), 0);
/// // Positive residual that overflows saturates to 255.
/// assert_eq!(reconstruct_sample(200, 100), 255);
/// ```
#[inline]
#[must_use]
pub const fn reconstruct_sample(pred: u8, residual: i32) -> u8 {
    // Widen to i64 so the add cannot overflow even for a pathological
    // residual at the i32 extremes; the clamp then bounds it to [0, 255].
    let sum = pred as i64 + residual as i64;
    let clamped = if sum < RECON_SAMPLE_MIN as i64 {
        RECON_SAMPLE_MIN
    } else if sum > RECON_SAMPLE_MAX as i64 {
        RECON_SAMPLE_MAX
    } else {
        sum as i32
    };
    clamped as u8
}

/// Compose a reconstructed 4×4 block from a predicted block and its
/// residual block, applying the spec/01 Gap 5 saturating clamp
/// element-wise.
///
/// This is the per-block form of [`reconstruct_sample`]: it walks the
/// two row-major `[_; 16]` blocks in lockstep and writes
/// `Clip1(pred[i] + residual[i])` at each position. Both inputs use
/// the same row-major 4×4 layout (`block[row * 4 + col]`) as
/// [`predict_diagonal_down_4x4`] and the
/// [`crate::svq3_dequant`] transform output, so the reconstructed
/// block is laid out the same way.
///
/// `predicted` is the intra/inter predictor output for the block;
/// `residual` is the dequantised, inverse-transformed coefficient
/// block from [`crate::svq3_dequant`] (already rounded by its fused
/// `+0x80000 … >> 20`). No additional rounding is applied to the sum,
/// per spec/01 Gap 5.
///
/// ```
/// use oxideav_svq::svq3_pred::{predict_diagonal_down_4x4, reconstruct_4x4};
///
/// // A uniform predictor plus an all-zero residual reproduces the
/// // prediction; a non-zero residual at a position shifts that sample.
/// let pred = predict_diagonal_down_4x4([5; 4], [5; 4]); // all 5s
/// let mut residual = [0i32; 16];
/// residual[0] = 10;
/// residual[15] = -100; // underflows -> clamps to 0
/// let recon = reconstruct_4x4(pred, residual);
/// assert_eq!(recon[0], 15);
/// assert_eq!(recon[1], 5);
/// assert_eq!(recon[15], 0);
/// ```
#[inline]
#[must_use]
pub const fn reconstruct_4x4(
    predicted: [u8; PRED_4X4_SAMPLES],
    residual: [i32; PRED_4X4_SAMPLES],
) -> [u8; PRED_4X4_SAMPLES] {
    let mut out = [0u8; PRED_4X4_SAMPLES];
    let mut i = 0;
    while i < PRED_4X4_SAMPLES {
        out[i] = reconstruct_sample(predicted[i], residual[i]);
        i += 1;
    }
    out
}

/// The five SVQ3 4×4 intra-prediction modes, numbered by the wire
/// value the intra-mode VLC resolves to.
///
/// Per `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 3
/// the mode value (`0..=4`, plus `-1` = unavailable) maps to the
/// H.264 intra-4×4 mode numbers:
///
/// | Value | H.264 intra-4×4 mode |
/// | ----- | -------------------- |
/// | 0 | Vertical (predict from top) |
/// | 1 | Horizontal (predict from left) |
/// | 2 | DC |
/// | 3 | Diagonal-Down-Left (SVQ3's `(left[k]+top[k])/2` quirk) |
/// | 4 | Diagonal-Down-Right |
///
/// Gap 3 pins value 3 to SVQ3's documented diagonal-down quirk (the
/// [`predict_diagonal_down_4x4`] predictor) and states modes 0/1/2/4
/// "follow their standard H.264 definitions … unmodified". It also
/// pins the default/fallback predictor used "for 16×16 intra and any
/// inter blocks" as value 2 (DC) — surfaced as [`Svq3IntraMode::DEFAULT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq3IntraMode {
    /// Mode value 0 — Vertical: each column copies the matching `top`
    /// sample.
    Vertical,
    /// Mode value 1 — Horizontal: each row copies the matching `left`
    /// sample.
    Horizontal,
    /// Mode value 2 — DC: every sample is the average of the available
    /// neighbour samples. This is also the default/fallback predictor
    /// per Gap 3.
    Dc,
    /// Mode value 3 — Diagonal-Down-Left, SVQ3's documented quirk:
    /// `(left[k] + top[k]) / 2` per the wiki §"Intra prediction"
    /// ([`predict_diagonal_down_4x4`]).
    DiagonalDownLeft,
    /// Mode value 4 — Diagonal-Down-Right (standard H.264).
    DiagonalDownRight,
}

impl Svq3IntraMode {
    /// The default/fallback intra-prediction mode — DC (value 2) — per
    /// `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 3:
    /// "the default/fallback predictor is value 2 (DC)". The wiki
    /// `pred_table` first entry is also `2`.
    pub const DEFAULT: Self = Self::Dc;

    /// Map the resolved wire value (`0..=4`) to the typed mode. Returns
    /// [`crate::Error::BadBitWidth`] for values outside `0..=4` (the
    /// SVQ3 4×4 intra-mode space, per spec/01 Gap 3 + the wiki
    /// `pred_table` range).
    pub const fn from_value(value: u8) -> crate::Result<Self> {
        match value {
            0 => Ok(Self::Vertical),
            1 => Ok(Self::Horizontal),
            2 => Ok(Self::Dc),
            3 => Ok(Self::DiagonalDownLeft),
            4 => Ok(Self::DiagonalDownRight),
            other => Err(crate::Error::BadBitWidth(other as u32)),
        }
    }

    /// The wire value (`0..=4`) for this mode — the inverse of
    /// [`Self::from_value`].
    pub const fn value(self) -> u8 {
        match self {
            Self::Vertical => 0,
            Self::Horizontal => 1,
            Self::Dc => 2,
            Self::DiagonalDownLeft => 3,
            Self::DiagonalDownRight => 4,
        }
    }
}

/// Neighbour samples a 4×4 intra predictor reads, with explicit
/// availability flags for the left column and top row.
///
/// The decode-side H.264 intra-4×4 predictors that spec/01 Gap 3
/// names as "standard H.264 … unmodified" (Vertical / Horizontal / DC
/// / Diagonal-Down-Right) read up to three neighbour groups:
///
/// * `top[0..=3]` — the four reconstructed samples directly above the
///   block (left-to-right).
/// * `left[0..=3]` — the four reconstructed samples directly to the
///   left of the block (top-to-bottom).
/// * `corner` — the reconstructed sample diagonally above-left of the
///   block (`p[-1, -1]`), read by the Diagonal-Down-Right predictor.
///
/// `top_available` / `left_available` mirror H.264's neighbour
/// availability: a block on the top edge of the slice has no `top`
/// row, a block on the left edge has no `left` column. The DC
/// predictor's averaging set shrinks accordingly (Gap 4 pins the same
/// availability-driven averaging for chroma DC; the luma 4×4 DC
/// predictor follows the identical standard-H.264 rule).
///
/// This carrier intentionally keeps the SVQ3 diagonal-down quirk
/// ([`predict_diagonal_down_4x4`]) on its own dedicated `left: [u8;4]`
/// / `top: [u8;4]` signature, since that predictor reads element `0`
/// of neither array; the dispatcher [`predict_intra_4x4`] bridges the
/// two conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intra4x4Neighbours {
    /// Top row `p[0..=3, -1]`, left-to-right.
    pub top: [u8; PRED_4X4_DIM],
    /// Left column `p[-1, 0..=3]`, top-to-bottom.
    pub left: [u8; PRED_4X4_DIM],
    /// Corner sample `p[-1, -1]` (above-left).
    pub corner: u8,
    /// Whether the `top` row is available (block not on the slice's
    /// top edge).
    pub top_available: bool,
    /// Whether the `left` column is available (block not on the
    /// slice's left edge).
    pub left_available: bool,
}

/// Standard H.264 4×4 **Vertical** predictor (mode 0): every sample in
/// a column copies the `top` sample above it.
///
/// `pred[row, col] = top[col]`. Per spec/01 Gap 3 this is the standard
/// H.264 Vertical mode, unmodified. Requires the top row to be
/// available; the dispatcher [`predict_intra_4x4`] is responsible for
/// only routing here when `top_available`.
#[must_use]
pub const fn predict_vertical_4x4(top: [u8; PRED_4X4_DIM]) -> [u8; PRED_4X4_SAMPLES] {
    let mut out = [0u8; PRED_4X4_SAMPLES];
    let mut row = 0;
    while row < PRED_4X4_DIM {
        let mut col = 0;
        while col < PRED_4X4_DIM {
            out[row * PRED_4X4_DIM + col] = top[col];
            col += 1;
        }
        row += 1;
    }
    out
}

/// Standard H.264 4×4 **Horizontal** predictor (mode 1): every sample
/// in a row copies the `left` sample beside it.
///
/// `pred[row, col] = left[row]`. Per spec/01 Gap 3 this is the
/// standard H.264 Horizontal mode, unmodified.
#[must_use]
pub const fn predict_horizontal_4x4(left: [u8; PRED_4X4_DIM]) -> [u8; PRED_4X4_SAMPLES] {
    let mut out = [0u8; PRED_4X4_SAMPLES];
    let mut row = 0;
    while row < PRED_4X4_DIM {
        let mut col = 0;
        while col < PRED_4X4_DIM {
            out[row * PRED_4X4_DIM + col] = left[row];
            col += 1;
        }
        row += 1;
    }
    out
}

/// Standard H.264 4×4 **DC** predictor (mode 2): every sample is the
/// rounded average of the available neighbour samples.
///
/// Per spec/01 Gap 3 (mode 2 = DC, "standard H.264 … unmodified") and
/// the standard H.264 4×4 DC rule, the predicted value is:
///
/// * both top and left available:
///   `(Σ top + Σ left + 4) >> 3`
/// * only top available: `(Σ top + 2) >> 2`
/// * only left available: `(Σ left + 2) >> 2`
/// * neither available: `128` (the mid-grey 8-bit default)
///
/// The single DC value is broadcast to all 16 positions.
#[must_use]
pub const fn predict_dc_4x4(
    top: [u8; PRED_4X4_DIM],
    left: [u8; PRED_4X4_DIM],
    top_available: bool,
    left_available: bool,
) -> [u8; PRED_4X4_SAMPLES] {
    let sum_top = top[0] as i32 + top[1] as i32 + top[2] as i32 + top[3] as i32;
    let sum_left = left[0] as i32 + left[1] as i32 + left[2] as i32 + left[3] as i32;
    let dc = if top_available && left_available {
        (sum_top + sum_left + 4) >> 3
    } else if top_available {
        (sum_top + 2) >> 2
    } else if left_available {
        (sum_left + 2) >> 2
    } else {
        128
    };
    [dc as u8; PRED_4X4_SAMPLES]
}

/// Standard H.264 4×4 **Diagonal-Down-Right** predictor (mode 4).
///
/// Per spec/01 Gap 3 mode 4 is the standard H.264 Diagonal-Down-Right
/// predictor, unmodified. The standard decode-side equations read the
/// top row `top[0..=3]`, the left column `left[0..=3]`, and the
/// above-left corner `corner` (`p[-1, -1]`), forming a 3-tap
/// `(a + 2b + c + 2) >> 2` filter along the main (top-left to
/// bottom-right) diagonal:
///
/// ```text
///   pred[x, y] (x = col, y = row)
///     x > y : (top[x-y-2]   + 2*top[x-y-1]   + top[x-y]   + 2) >> 2
///     x < y : (left[y-x-2]  + 2*left[y-x-1]  + left[y-x]  + 2) >> 2
///     x = y : (top[0]       + 2*corner       + left[0]    + 2) >> 2
/// ```
///
/// where the `index = -1` tap reads the `corner` sample. Requires both
/// the top row and left column (and the corner) to be available; the
/// dispatcher [`predict_intra_4x4`] only routes here when both
/// neighbours are present.
#[must_use]
pub const fn predict_diagonal_down_right_4x4(nb: Intra4x4Neighbours) -> [u8; PRED_4X4_SAMPLES] {
    // Helper closures aren't allowed in const fn; index the four taps
    // manually. tap(k) reads top[k] for k in 0..=3, left[-1] = corner
    // is handled by the per-branch corner read below.
    let mut out = [0u8; PRED_4X4_SAMPLES];
    let mut y = 0; // row
    while y < PRED_4X4_DIM {
        let mut x = 0; // col
        while x < PRED_4X4_DIM {
            let v = if x > y {
                // diff = x - y >= 1; taps at top[diff-2], top[diff-1], top[diff]
                let diff = x - y;
                let t0 = if diff >= 2 {
                    nb.top[diff - 2] as i32
                } else {
                    nb.corner as i32
                };
                let t1 = nb.top[diff - 1] as i32;
                let t2 = nb.top[diff] as i32;
                (t0 + 2 * t1 + t2 + 2) >> 2
            } else if x < y {
                let diff = y - x;
                let l0 = if diff >= 2 {
                    nb.left[diff - 2] as i32
                } else {
                    nb.corner as i32
                };
                let l1 = nb.left[diff - 1] as i32;
                let l2 = nb.left[diff] as i32;
                (l0 + 2 * l1 + l2 + 2) >> 2
            } else {
                // x == y: main diagonal sample.
                (nb.top[0] as i32 + 2 * nb.corner as i32 + nb.left[0] as i32 + 2) >> 2
            };
            out[y * PRED_4X4_DIM + x] = v as u8;
            x += 1;
        }
        y += 1;
    }
    out
}

/// Predict one 4×4 intra block by dispatching on the resolved
/// [`Svq3IntraMode`].
///
/// This is the **mode-to-predictor binding** the README named as a
/// lacks-tail item: spec/01 Gap 3 pins each mode value `0..=4` to a
/// predictor, and this dispatcher routes a resolved mode to the
/// matching block predictor, supplying the neighbour samples from
/// `nb`.
///
/// Edge handling follows the standard H.264 fallback that Gap 3
/// references: a directional predictor whose required neighbour is
/// unavailable falls back to DC, which is also the documented default
/// predictor ([`Svq3IntraMode::DEFAULT`]). Specifically:
///
/// * [`Svq3IntraMode::Vertical`] needs `top`; falls back to DC if
///   `!top_available`.
/// * [`Svq3IntraMode::Horizontal`] needs `left`; falls back to DC if
///   `!left_available`.
/// * [`Svq3IntraMode::DiagonalDownLeft`] (SVQ3's quirk) and
///   [`Svq3IntraMode::DiagonalDownRight`] need both neighbours; they
///   fall back to DC if either is missing.
/// * [`Svq3IntraMode::Dc`] adapts its averaging set to whichever
///   neighbours are available (and yields `128` with neither).
///
/// The diagonal-down-left quirk reads `left[1..=3]` / `top[1..=3]`
/// (element 0 unused, per the wiki), so the dispatcher forwards
/// `nb.left` / `nb.top` directly to [`predict_diagonal_down_4x4`].
#[must_use]
pub const fn predict_intra_4x4(
    mode: Svq3IntraMode,
    nb: Intra4x4Neighbours,
) -> [u8; PRED_4X4_SAMPLES] {
    match mode {
        Svq3IntraMode::Vertical => {
            if nb.top_available {
                predict_vertical_4x4(nb.top)
            } else {
                predict_dc_4x4(nb.top, nb.left, nb.top_available, nb.left_available)
            }
        }
        Svq3IntraMode::Horizontal => {
            if nb.left_available {
                predict_horizontal_4x4(nb.left)
            } else {
                predict_dc_4x4(nb.top, nb.left, nb.top_available, nb.left_available)
            }
        }
        Svq3IntraMode::Dc => predict_dc_4x4(nb.top, nb.left, nb.top_available, nb.left_available),
        Svq3IntraMode::DiagonalDownLeft => {
            if nb.top_available && nb.left_available {
                predict_diagonal_down_4x4(nb.left, nb.top)
            } else {
                predict_dc_4x4(nb.top, nb.left, nb.top_available, nb.left_available)
            }
        }
        Svq3IntraMode::DiagonalDownRight => {
            if nb.top_available && nb.left_available {
                predict_diagonal_down_right_4x4(nb)
            } else {
                predict_dc_4x4(nb.top, nb.left, nb.top_available, nb.left_available)
            }
        }
    }
}

/// Width / height of a 16×16 luma macroblock prediction block.
pub const PRED_16X16_DIM: usize = 16;

/// Number of samples in one 16×16 predicted block.
pub const PRED_16X16_SAMPLES: usize = PRED_16X16_DIM * PRED_16X16_DIM;

/// The SVQ3 16×16 luma **plane** predictor — the standard H.264 plane
/// prediction "but transposed", per
/// `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 4.
///
/// Gap 4 pins the decode-side equations. With the top row `top[0..=15]`
/// (`top[x]`, x = 0..15) and left column `left[0..=15]` (`left[y]`,
/// y = 0..15):
///
/// ```text
///   H = Σ_{x'=1..7} x' · ( top[7+x']  − top[7−x']  )
///   V = Σ_{y'=1..7} y' · ( left[7+y'] − left[7−y'] )
///   a = 16 · ( top[15] + left[15] )
///   b = (5·H + 32) >> 6
///   c = (5·V + 32) >> 6
/// ```
///
/// Standard H.264 writes the plane back as
/// `Clip1( (a + b·(x−7) + c·(y−7) + 16) >> 5 )`; SVQ3's documented
/// **transpose** swaps the per-pixel coordinate roles so `b` is applied
/// along `y` and `c` along `x`:
///
/// ```text
///   pred[x, y] = Clip1( (a + b·(y−7) + c·(x−7) + 16) >> 5 )
/// ```
///
/// (`x` = col, `y` = row). All constants (`5`, `32`, `>>6`, `16`,
/// `>>5`) are the standard H.264 plane constants per Gap 4. The output
/// is row-major (`out[y * 16 + x]`). The `Clip1` clamp is the same
/// 8-bit `[0, 255]` saturation as [`reconstruct_sample`]
/// ([`RECON_SAMPLE_MIN`] / [`RECON_SAMPLE_MAX`]).
///
/// This predictor requires both the top row and the left column. The
/// caller (macroblock loop) only selects the plane predictor for an
/// interior macroblock where both are available; for an edge macroblock
/// the 16×16 DC predictor [`predict_dc_16x16`] is used instead.
#[must_use]
pub const fn predict_plane_16x16(
    top: [u8; PRED_16X16_DIM],
    left: [u8; PRED_16X16_DIM],
) -> [u8; PRED_16X16_SAMPLES] {
    // H = Σ_{x'=1..7} x' · (top[7+x'] − top[7−x'])
    let mut h: i32 = 0;
    let mut k = 1;
    while k <= 7 {
        h += (k as i32) * (top[7 + k] as i32 - top[7 - k] as i32);
        k += 1;
    }
    // V = Σ_{y'=1..7} y' · (left[7+y'] − left[7−y'])
    let mut v: i32 = 0;
    k = 1;
    while k <= 7 {
        v += (k as i32) * (left[7 + k] as i32 - left[7 - k] as i32);
        k += 1;
    }
    let a = 16 * (top[15] as i32 + left[15] as i32);
    let b = (5 * h + 32) >> 6;
    let c = (5 * v + 32) >> 6;

    let mut out = [0u8; PRED_16X16_SAMPLES];
    let mut y = 0; // row
    while y < PRED_16X16_DIM {
        let mut x = 0; // col
        while x < PRED_16X16_DIM {
            // SVQ3 transpose: b along y, c along x.
            let raw = (a + b * (y as i32 - 7) + c * (x as i32 - 7) + 16) >> 5;
            let clamped = if raw < RECON_SAMPLE_MIN {
                RECON_SAMPLE_MIN
            } else if raw > RECON_SAMPLE_MAX {
                RECON_SAMPLE_MAX
            } else {
                raw
            };
            out[y * PRED_16X16_DIM + x] = clamped as u8;
            x += 1;
        }
        y += 1;
    }
    out
}

/// The 16×16 luma **DC** predictor — the standard H.264 16×16 DC mode
/// used by SVQ3 at macroblock edges where the plane predictor's
/// neighbours are not both available.
///
/// Follows the same availability-driven averaging rule as the 4×4 DC
/// predictor (Gap 3 / standard H.264), scaled to the 16-sample
/// neighbour rows:
///
/// * both available: `(Σ top + Σ left + 16) >> 5`
/// * only top: `(Σ top + 8) >> 4`
/// * only left: `(Σ left + 8) >> 4`
/// * neither: `128`
///
/// The single DC value is broadcast to all 256 positions (row-major).
#[must_use]
pub fn predict_dc_16x16(
    top: [u8; PRED_16X16_DIM],
    left: [u8; PRED_16X16_DIM],
    top_available: bool,
    left_available: bool,
) -> [u8; PRED_16X16_SAMPLES] {
    let mut sum_top: i32 = 0;
    let mut sum_left: i32 = 0;
    for i in 0..PRED_16X16_DIM {
        sum_top += top[i] as i32;
        sum_left += left[i] as i32;
    }
    let dc = if top_available && left_available {
        (sum_top + sum_left + 16) >> 5
    } else if top_available {
        (sum_top + 8) >> 4
    } else if left_available {
        (sum_left + 8) >> 4
    } else {
        128
    };
    [dc as u8; PRED_16X16_SAMPLES]
}

/// Width / height of one 8×8 chroma prediction block (one chroma
/// plane of a macroblock).
pub const PRED_CHROMA_DIM: usize = 8;

/// Number of samples in one 8×8 chroma predicted block.
pub const PRED_CHROMA_SAMPLES: usize = PRED_CHROMA_DIM * PRED_CHROMA_DIM;

/// The SVQ3 8×8 chroma predictor — **DC mode only**, per
/// `docs/video/svq3/spec/01-reconstruction-composition.md` Gap 4
/// ("SVQ3 forces chroma to DC mode only (no chroma plane / vertical /
/// horizontal selection)").
///
/// Gap 4 pins the per-4×4-quadrant DC value via the standard H.264
/// chroma-DC averaging over available neighbours. Each of the four 4×4
/// quadrants of the 8×8 block averages its own 4 top samples and 4
/// left samples:
///
/// ```text
///   if both top and left available:  dc = (Σ top[0..3] + Σ left[0..3] + 4) >> 3
///   elif only top available:         dc = (Σ top[0..3] + 2) >> 2
///   elif only left available:        dc = (Σ left[0..3] + 2) >> 2
///   else:                            dc = 128
/// ```
///
/// applied per the four 4×4 chroma-DC quadrants exactly as H.264
/// chroma DC. `top[0..=7]` / `left[0..=7]` are the 8 reconstructed
/// neighbour samples above / to the left of the 8×8 chroma block.
/// For quadrant `(qr, qc)` (qr, qc ∈ {0, 1}) the top group is
/// `top[qc*4 .. qc*4+4]` and the left group is
/// `left[qr*4 .. qr*4+4]`. The single quadrant DC value fills the
/// quadrant's 4×4 samples; the four quadrant values are written into
/// the row-major 8×8 output (`out[y * 8 + x]`).
#[must_use]
pub fn predict_chroma_dc_8x8(
    top: [u8; PRED_CHROMA_DIM],
    left: [u8; PRED_CHROMA_DIM],
    top_available: bool,
    left_available: bool,
) -> [u8; PRED_CHROMA_SAMPLES] {
    // Per-quadrant DC: sum the 4 top samples / 4 left samples of the
    // quadrant, then the availability-driven rounding.
    let quad_dc = |top4: i32, left4: i32| -> u8 {
        let dc = if top_available && left_available {
            (top4 + left4 + 4) >> 3
        } else if top_available {
            (top4 + 2) >> 2
        } else if left_available {
            (left4 + 2) >> 2
        } else {
            128
        };
        dc as u8
    };
    let group_sum = |arr: &[u8; PRED_CHROMA_DIM], base: usize| -> i32 {
        arr[base] as i32 + arr[base + 1] as i32 + arr[base + 2] as i32 + arr[base + 3] as i32
    };

    let mut out = [0u8; PRED_CHROMA_SAMPLES];
    for y in 0..PRED_CHROMA_DIM {
        let qr = y / 4; // quadrant row
        let left4 = group_sum(&left, qr * 4);
        for x in 0..PRED_CHROMA_DIM {
            let qc = x / 4; // quadrant col
            let top4 = group_sum(&top, qc * 4);
            out[y * PRED_CHROMA_DIM + x] = quad_dc(top4, left4);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_matches_spec_picture() {
        // Row-major transcription of the wiki picture:
        //   a b c c / b c c c / c c c c / c c c c
        #[rustfmt::skip]
        let expected: [u8; 16] = [
            0, 1, 2, 2,
            1, 2, 2, 2,
            2, 2, 2, 2,
            2, 2, 2, 2,
        ];
        assert_eq!(DIAGONAL_DOWN_PATTERN, expected);
    }

    #[test]
    fn pattern_kind_population() {
        // The picture places `a` once, `b` twice, `c` thirteen times.
        let mut counts = [0usize; 3];
        for &kind in DIAGONAL_DOWN_PATTERN.iter() {
            assert!(kind <= 2, "pattern entry out of range: {kind}");
            counts[kind as usize] += 1;
        }
        assert_eq!(counts, [1, 2, 13]);
    }

    #[test]
    fn neighbour_indices_are_one_two_three() {
        assert_eq!(DIAGONAL_DOWN_NEIGHBOUR_INDICES, [1, 2, 3]);
        for &idx in DIAGONAL_DOWN_NEIGHBOUR_INDICES.iter() {
            assert!(idx < PRED_4X4_DIM);
        }
    }

    #[test]
    fn dimension_constants_agree() {
        assert_eq!(PRED_4X4_DIM, 4);
        assert_eq!(PRED_4X4_SAMPLES, 16);
        assert_eq!(DIAGONAL_DOWN_PATTERN.len(), PRED_4X4_SAMPLES);
    }

    #[test]
    fn sample_zero_and_max_bounds() {
        assert_eq!(diagonal_down_sample(0, 0), 0);
        assert_eq!(diagonal_down_sample(255, 255), 255);
        assert_eq!(diagonal_down_sample(255, 0), 127);
        assert_eq!(diagonal_down_sample(0, 255), 127);
    }

    #[test]
    fn sample_floor_division() {
        // The spec writes a plain `/ 2`: odd sums floor.
        assert_eq!(diagonal_down_sample(1, 2), 1);
        assert_eq!(diagonal_down_sample(2, 1), 1);
        assert_eq!(diagonal_down_sample(0, 1), 0);
        assert_eq!(diagonal_down_sample(3, 0), 1);
        assert_eq!(diagonal_down_sample(10, 20), 15);
        assert_eq!(diagonal_down_sample(61, 0), 30);
    }

    #[test]
    fn sample_is_symmetric_in_left_and_top() {
        let sweep = [0u8, 1, 2, 61, 127, 128, 200, 254, 255];
        for &l in sweep.iter() {
            for &t in sweep.iter() {
                assert_eq!(
                    diagonal_down_sample(l, t),
                    diagonal_down_sample(t, l),
                    "asymmetric at ({l}, {t})"
                );
            }
        }
    }

    #[test]
    fn predict_uniform_neighbours_reproduce_the_value() {
        for &v in [0u8, 1, 61, 127, 200, 255].iter() {
            let out = predict_diagonal_down_4x4([v; 4], [v; 4]);
            assert_eq!(out, [v; 16], "uniform value {v} not reproduced");
        }
    }

    #[test]
    fn predict_matches_the_three_closed_forms() {
        let left = [200u8, 17, 48, 99];
        let top = [3u8, 250, 5, 130];
        let a = diagonal_down_sample(left[1], top[1]);
        let b = diagonal_down_sample(left[2], top[2]);
        let c = diagonal_down_sample(left[3], top[3]);
        let out = predict_diagonal_down_4x4(left, top);
        assert_eq!(out[0], a);
        assert_eq!(out[1], b);
        assert_eq!(out[4], b);
        for (i, &sample) in out.iter().enumerate() {
            if i != 0 && i != 1 && i != 4 {
                assert_eq!(sample, c, "position {i} should carry c");
            }
        }
    }

    #[test]
    fn predict_row_layout_matches_picture() {
        let left = [0u8, 10, 30, 50];
        let top = [0u8, 14, 32, 52];
        let a = 12; // (10 + 14) / 2
        let b = 31; // (30 + 32) / 2
        let c = 51; // (50 + 52) / 2
        let out = predict_diagonal_down_4x4(left, top);
        let rows: [[u8; 4]; 4] = [[a, b, c, c], [b, c, c, c], [c, c, c, c], [c, c, c, c]];
        for (r, row) in rows.iter().enumerate() {
            for (col, &expected) in row.iter().enumerate() {
                assert_eq!(out[r * 4 + col], expected, "mismatch at ({r}, {col})");
            }
        }
    }

    #[test]
    fn predict_ignores_neighbour_index_zero() {
        let left = [0u8, 11, 22, 33];
        let top = [0u8, 44, 55, 66];
        let baseline = predict_diagonal_down_4x4(left, top);
        for &noise in [1u8, 128, 255].iter() {
            let mut l = left;
            let mut t = top;
            l[0] = noise;
            t[0] = noise.wrapping_add(7);
            assert_eq!(
                predict_diagonal_down_4x4(l, t),
                baseline,
                "element 0 leaked into the prediction (noise {noise})"
            );
        }
    }

    #[test]
    fn predict_is_symmetric_in_left_and_top() {
        let left = [5u8, 90, 180, 240];
        let top = [200u8, 15, 60, 1];
        assert_eq!(
            predict_diagonal_down_4x4(left, top),
            predict_diagonal_down_4x4(top, left)
        );
    }

    #[test]
    fn predict_agrees_with_pattern_indexing() {
        let left = [77u8, 1, 254, 128];
        let top = [12u8, 255, 0, 129];
        let derived = [
            diagonal_down_sample(left[1], top[1]),
            diagonal_down_sample(left[2], top[2]),
            diagonal_down_sample(left[3], top[3]),
        ];
        let out = predict_diagonal_down_4x4(left, top);
        for (i, &sample) in out.iter().enumerate() {
            assert_eq!(sample, derived[DIAGONAL_DOWN_PATTERN[i] as usize]);
        }
    }

    #[test]
    fn predict_worked_example() {
        let out = predict_diagonal_down_4x4([9, 10, 20, 30], [7, 14, 21, 31]);
        #[rustfmt::skip]
        let expected: [u8; 16] = [
            12, 20, 30, 30,
            20, 30, 30, 30,
            30, 30, 30, 30,
            30, 30, 30, 30,
        ];
        assert_eq!(out, expected);
    }

    #[test]
    fn helpers_are_const_usable() {
        const SAMPLE: u8 = diagonal_down_sample(30, 31);
        const BLOCK: [u8; PRED_4X4_SAMPLES] = predict_diagonal_down_4x4([0, 2, 4, 6], [0, 2, 4, 6]);
        assert_eq!(SAMPLE, 30);
        assert_eq!(BLOCK[0], 2);
        assert_eq!(BLOCK[1], 4);
        assert_eq!(BLOCK[15], 6);
    }

    // ---- spec/01 Gap 5: predicted+residual writeback composition -----

    #[test]
    fn recon_clamp_bounds_match_8bit() {
        assert_eq!(RECON_SAMPLE_MIN, 0);
        assert_eq!(RECON_SAMPLE_MAX, 255);
        assert_eq!(RECON_SAMPLE_MAX, (1i32 << 8) - 1);
    }

    #[test]
    fn recon_sample_in_range_is_plain_sum() {
        // No rounding term on the add — the in-range case is the exact
        // signed sum pred + residual.
        assert_eq!(reconstruct_sample(0, 0), 0);
        assert_eq!(reconstruct_sample(100, 27), 127);
        assert_eq!(reconstruct_sample(255, 0), 255);
        assert_eq!(reconstruct_sample(128, -28), 100);
        assert_eq!(reconstruct_sample(0, 255), 255);
    }

    #[test]
    fn recon_sample_saturates_low() {
        assert_eq!(reconstruct_sample(10, -50), 0);
        assert_eq!(reconstruct_sample(0, -1), 0);
        assert_eq!(reconstruct_sample(0, i32::MIN), 0);
        assert_eq!(reconstruct_sample(127, -128), 0); // exactly 0, not below
        assert_eq!(reconstruct_sample(127, -127), 0);
    }

    #[test]
    fn recon_sample_saturates_high() {
        assert_eq!(reconstruct_sample(200, 100), 255);
        assert_eq!(reconstruct_sample(255, 1), 255);
        assert_eq!(reconstruct_sample(255, i32::MAX), 255);
        assert_eq!(reconstruct_sample(200, 55), 255); // exactly 255
        assert_eq!(reconstruct_sample(200, 56), 255); // one over -> clamps
    }

    #[test]
    fn recon_sample_zero_residual_is_identity() {
        for pred in 0u8..=255 {
            assert_eq!(reconstruct_sample(pred, 0), pred, "pred {pred}");
        }
    }

    #[test]
    fn recon_4x4_zero_residual_reproduces_prediction() {
        let pred = predict_diagonal_down_4x4([9, 10, 20, 30], [7, 14, 21, 31]);
        let recon = reconstruct_4x4(pred, [0i32; PRED_4X4_SAMPLES]);
        assert_eq!(recon, pred);
    }

    #[test]
    fn recon_4x4_is_elementwise_clamped_sum() {
        let pred = predict_diagonal_down_4x4([5; 4], [5; 4]); // all 5s
        let mut residual = [0i32; PRED_4X4_SAMPLES];
        residual[0] = 10;
        residual[7] = 250; // 5 + 250 = 255
        residual[8] = 251; // 5 + 251 = 256 -> clamps to 255
        residual[15] = -100; // 5 - 100 -> clamps to 0
        let recon = reconstruct_4x4(pred, residual);
        assert_eq!(recon[0], 15);
        assert_eq!(recon[1], 5);
        assert_eq!(recon[7], 255);
        assert_eq!(recon[8], 255);
        assert_eq!(recon[15], 0);
        // Every other position is the untouched prediction (5).
        for i in [2usize, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14] {
            assert_eq!(recon[i], 5, "position {i}");
        }
    }

    #[test]
    fn recon_4x4_matches_per_sample_helper() {
        let pred = predict_diagonal_down_4x4([200, 17, 48, 99], [3, 250, 5, 130]);
        let residual: [i32; PRED_4X4_SAMPLES] = [
            0, 5, -300, 400, 1, -1, 127, -127, 255, -255, 12, -12, 50, -50, 200, -200,
        ];
        let block = reconstruct_4x4(pred, residual);
        for i in 0..PRED_4X4_SAMPLES {
            assert_eq!(
                block[i],
                reconstruct_sample(pred[i], residual[i]),
                "position {i}"
            );
        }
    }

    #[test]
    fn recon_helpers_are_const_usable() {
        const SAMPLE: u8 = reconstruct_sample(100, 27);
        const BLOCK: [u8; PRED_4X4_SAMPLES] = reconstruct_4x4([5u8; 16], [10i32; 16]);
        assert_eq!(SAMPLE, 127);
        assert_eq!(BLOCK, [15u8; 16]);
    }

    // ---- intra-mode binding + standard H.264 4×4 predictors ----------

    fn nb(top: [u8; 4], left: [u8; 4], corner: u8) -> Intra4x4Neighbours {
        Intra4x4Neighbours {
            top,
            left,
            corner,
            top_available: true,
            left_available: true,
        }
    }

    #[test]
    fn intra_mode_value_round_trip() {
        for v in 0u8..=4 {
            let m = Svq3IntraMode::from_value(v).unwrap();
            assert_eq!(m.value(), v);
        }
        assert!(matches!(
            Svq3IntraMode::from_value(5),
            Err(crate::Error::BadBitWidth(5))
        ));
        assert!(matches!(
            Svq3IntraMode::from_value(255),
            Err(crate::Error::BadBitWidth(255))
        ));
    }

    #[test]
    fn intra_mode_binding_matches_gap3() {
        // Gap 3: 0=Vertical, 1=Horizontal, 2=DC, 3=DiagDownLeft, 4=DiagDownRight.
        assert_eq!(
            Svq3IntraMode::from_value(0).unwrap(),
            Svq3IntraMode::Vertical
        );
        assert_eq!(
            Svq3IntraMode::from_value(1).unwrap(),
            Svq3IntraMode::Horizontal
        );
        assert_eq!(Svq3IntraMode::from_value(2).unwrap(), Svq3IntraMode::Dc);
        assert_eq!(
            Svq3IntraMode::from_value(3).unwrap(),
            Svq3IntraMode::DiagonalDownLeft
        );
        assert_eq!(
            Svq3IntraMode::from_value(4).unwrap(),
            Svq3IntraMode::DiagonalDownRight
        );
        // Gap 3: default/fallback predictor is value 2 (DC).
        assert_eq!(Svq3IntraMode::DEFAULT, Svq3IntraMode::Dc);
        assert_eq!(Svq3IntraMode::DEFAULT.value(), 2);
    }

    #[test]
    fn vertical_copies_top_down_each_column() {
        let top = [10u8, 20, 30, 40];
        let out = predict_vertical_4x4(top);
        for row in 0..4 {
            for col in 0..4 {
                assert_eq!(out[row * 4 + col], top[col], "({row},{col})");
            }
        }
    }

    #[test]
    fn horizontal_copies_left_across_each_row() {
        let left = [11u8, 22, 33, 44];
        let out = predict_horizontal_4x4(left);
        for row in 0..4 {
            for col in 0..4 {
                assert_eq!(out[row * 4 + col], left[row], "({row},{col})");
            }
        }
    }

    #[test]
    fn dc_both_available_rounds_sum_of_eight() {
        // Σtop = 1+2+3+4 = 10, Σleft = 5+6+7+8 = 26, total 36; (36+4)>>3 = 5.
        let out = predict_dc_4x4([1, 2, 3, 4], [5, 6, 7, 8], true, true);
        assert_eq!(out, [5u8; 16]);
    }

    #[test]
    fn dc_only_top_or_only_left() {
        // Only top: (Σtop + 2) >> 2. Σtop = 40 → (42)>>2 = 10.
        let out = predict_dc_4x4([10, 10, 10, 10], [99, 99, 99, 99], true, false);
        assert_eq!(out, [10u8; 16]);
        // Only left: (Σleft + 2) >> 2. Σleft = 40 → 10.
        let out = predict_dc_4x4([99, 99, 99, 99], [10, 10, 10, 10], false, true);
        assert_eq!(out, [10u8; 16]);
    }

    #[test]
    fn dc_neither_available_is_128() {
        let out = predict_dc_4x4([7; 4], [7; 4], false, false);
        assert_eq!(out, [128u8; 16]);
    }

    #[test]
    fn diagonal_down_right_uniform_neighbours_reproduce_value() {
        // All neighbours (incl corner) equal v → every 3-tap average is v.
        for &v in [0u8, 1, 63, 127, 200, 255].iter() {
            let out = predict_diagonal_down_right_4x4(nb([v; 4], [v; 4], v));
            assert_eq!(out, [v; 16], "uniform {v}");
        }
    }

    #[test]
    fn diagonal_down_right_main_diagonal_uses_corner() {
        // top = [t0..], left = [l0..], corner = c.
        // Main diagonal sample = (top[0] + 2*corner + left[0] + 2) >> 2.
        let top = [40u8, 0, 0, 0];
        let left = [80u8, 0, 0, 0];
        let corner = 100u8;
        let out = predict_diagonal_down_right_4x4(nb(top, left, corner));
        let expected = ((40 + 2 * 100 + 80 + 2) >> 2) as u8; // (322)>>2 = 80
        for d in 0..4 {
            assert_eq!(out[d * 4 + d], expected, "diagonal {d}");
        }
    }

    #[test]
    fn diagonal_down_right_upper_triangle_reads_top() {
        // For x = col, y = row with x > y, pred uses top[x-y-2..x-y].
        // Pick (row=0, col=2): diff=2 → (top[0]+2*top[1]+top[2]+2)>>2.
        let top = [4u8, 8, 12, 16];
        let left = [0u8; 4];
        let out = predict_diagonal_down_right_4x4(nb(top, left, 0));
        let expected = ((4 + 2 * 8 + 12 + 2) >> 2) as u8; // (34)>>2 = 8
        assert_eq!(out[2], expected); // row 0, col 2
    }

    #[test]
    fn dispatcher_routes_each_mode() {
        let n = nb([10, 20, 30, 40], [50, 60, 70, 80], 5);
        assert_eq!(
            predict_intra_4x4(Svq3IntraMode::Vertical, n),
            predict_vertical_4x4(n.top)
        );
        assert_eq!(
            predict_intra_4x4(Svq3IntraMode::Horizontal, n),
            predict_horizontal_4x4(n.left)
        );
        assert_eq!(
            predict_intra_4x4(Svq3IntraMode::Dc, n),
            predict_dc_4x4(n.top, n.left, true, true)
        );
        assert_eq!(
            predict_intra_4x4(Svq3IntraMode::DiagonalDownLeft, n),
            predict_diagonal_down_4x4(n.left, n.top)
        );
        assert_eq!(
            predict_intra_4x4(Svq3IntraMode::DiagonalDownRight, n),
            predict_diagonal_down_right_4x4(n)
        );
    }

    #[test]
    fn dispatcher_falls_back_to_dc_when_neighbour_missing() {
        // Vertical with no top → DC over left only.
        let n = Intra4x4Neighbours {
            top: [99; 4],
            left: [10, 10, 10, 10],
            corner: 0,
            top_available: false,
            left_available: true,
        };
        let got = predict_intra_4x4(Svq3IntraMode::Vertical, n);
        assert_eq!(got, predict_dc_4x4(n.top, n.left, false, true));
        assert_eq!(got, [10u8; 16]);

        // Horizontal with no left → DC over top only.
        let n2 = Intra4x4Neighbours {
            top: [20, 20, 20, 20],
            left: [99; 4],
            corner: 0,
            top_available: true,
            left_available: false,
        };
        let got2 = predict_intra_4x4(Svq3IntraMode::Horizontal, n2);
        assert_eq!(got2, [20u8; 16]);

        // Both diagonals with a missing neighbour → DC fallback.
        for mode in [
            Svq3IntraMode::DiagonalDownLeft,
            Svq3IntraMode::DiagonalDownRight,
        ] {
            let n3 = Intra4x4Neighbours {
                top: [30, 30, 30, 30],
                left: [99; 4],
                corner: 7,
                top_available: true,
                left_available: false,
            };
            assert_eq!(
                predict_intra_4x4(mode, n3),
                predict_dc_4x4(n3.top, n3.left, true, false)
            );
        }
    }

    #[test]
    fn predictors_are_const_usable() {
        const N: Intra4x4Neighbours = Intra4x4Neighbours {
            top: [1, 2, 3, 4],
            left: [5, 6, 7, 8],
            corner: 9,
            top_available: true,
            left_available: true,
        };
        const V: [u8; 16] = predict_vertical_4x4(N.top);
        const DC: [u8; 16] = predict_dc_4x4(N.top, N.left, true, true);
        const DDR: [u8; 16] = predict_diagonal_down_right_4x4(N);
        const DISP: [u8; 16] = predict_intra_4x4(Svq3IntraMode::Dc, N);
        assert_eq!(V[0], 1);
        assert_eq!(DC, [5u8; 16]); // (10+26+4)>>3 = 5
        assert_eq!(DISP, DC);
        assert_eq!(DDR[0], DDR[0]); // smoke
    }

    // ---- 16×16 plane / DC + chroma DC (spec/01 Gap 4) ----------------

    #[test]
    fn plane16_uniform_neighbours_reproduce_value() {
        // Uniform neighbours → H = V = 0, a = 16*2v = 32v,
        // pred = (32v + 16) >> 5 = v (for v in 0..=255, the +16 rounds
        // 32v/32 exactly to v).
        for &v in [0u8, 1, 50, 100, 128, 200, 255].iter() {
            let out = predict_plane_16x16([v; 16], [v; 16]);
            assert_eq!(out, [v; 256], "uniform {v}");
        }
    }

    #[test]
    fn plane16_dim_constants() {
        assert_eq!(PRED_16X16_DIM, 16);
        assert_eq!(PRED_16X16_SAMPLES, 256);
    }

    #[test]
    fn plane16_transpose_b_along_y_c_along_x() {
        // Construct neighbours that give a known H, V and verify the
        // transposed application: b applied along y, c along x.
        // Use a horizontal ramp in `top` and flat `left`.
        let mut top = [0u8; 16];
        for (i, t) in top.iter_mut().enumerate() {
            *t = (8 * i) as u8; // 0,8,16,...,120
        }
        let left = [60u8; 16];
        let out = predict_plane_16x16(top, left);

        // Recompute b, c the same way the function does.
        let mut h = 0i32;
        for k in 1..=7i32 {
            h += k * (top[7 + k as usize] as i32 - top[7 - k as usize] as i32);
        }
        let v = 0i32; // left flat
        let a = 16 * (top[15] as i32 + left[15] as i32);
        let b = (5 * h + 32) >> 6;
        let c = (5 * v + 32) >> 6;
        // c == 0 (V == 0), so prediction varies only along y (via b).
        assert_eq!(c, 0);
        // Spot-check (x=3, y=5): (a + b*(5-7) + c*(3-7) + 16) >> 5.
        let raw = (a + b * (5 - 7) + c * (3 - 7) + 16) >> 5;
        let expected = raw.clamp(0, 255) as u8;
        assert_eq!(out[5 * 16 + 3], expected);
    }

    #[test]
    fn dc16_both_available() {
        // Σtop = 16*10 = 160, Σleft = 16*22 = 352, total 512;
        // (512 + 16) >> 5 = 16.
        let out = predict_dc_16x16([10; 16], [22; 16], true, true);
        assert_eq!(out, [16u8; 256]);
    }

    #[test]
    fn dc16_partial_and_none() {
        // Only top: (Σtop + 8) >> 4. Σtop = 16*16 = 256 → (264)>>4 = 16.
        let out = predict_dc_16x16([16; 16], [99; 16], true, false);
        assert_eq!(out, [16u8; 256]);
        // Only left.
        let out = predict_dc_16x16([99; 16], [16; 16], false, true);
        assert_eq!(out, [16u8; 256]);
        // Neither → 128.
        let out = predict_dc_16x16([3; 16], [3; 16], false, false);
        assert_eq!(out, [128u8; 256]);
    }

    #[test]
    fn chroma_dc_dim_constants() {
        assert_eq!(PRED_CHROMA_DIM, 8);
        assert_eq!(PRED_CHROMA_SAMPLES, 64);
    }

    #[test]
    fn chroma_dc_uniform_both_available() {
        // top4 = left4 = 4*v; (4v + 4v + 4) >> 3 = v (for v even-ish).
        // Use v = 10: (40 + 40 + 4) >> 3 = 10.
        let out = predict_chroma_dc_8x8([10; 8], [10; 8], true, true);
        assert_eq!(out, [10u8; 64]);
    }

    #[test]
    fn chroma_dc_per_quadrant_independence() {
        // top: left half (cols 0..3) = 0, right half (cols 4..7) = 40.
        // left: top half (rows 0..3) = 80, bottom half (rows 4..7) = 0.
        let top = [0, 0, 0, 0, 40, 40, 40, 40];
        let left = [80, 80, 80, 80, 0, 0, 0, 0];
        let out = predict_chroma_dc_8x8(top, left, true, true);
        // Quadrant (qr=0,qc=0): top4 = 0, left4 = 320 →
        //   (0 + 320 + 4) >> 3 = 40.
        assert_eq!(out[0], 40);
        // Quadrant (qr=0,qc=1): top4 = 160, left4 = 320 →
        //   (160 + 320 + 4) >> 3 = 60.
        assert_eq!(out[4], 60);
        // Quadrant (qr=1,qc=0): top4 = 0, left4 = 0 → 0.
        assert_eq!(out[4 * 8], 0);
        // Quadrant (qr=1,qc=1): top4 = 160, left4 = 0 →
        //   (160 + 0 + 4) >> 3 = 20.
        assert_eq!(out[4 * 8 + 4], 20);
        // All samples within a quadrant share the same value.
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(out[y * 8 + x], 40, "Q00 ({y},{x})");
            }
        }
    }

    #[test]
    fn chroma_dc_neither_available_is_128() {
        let out = predict_chroma_dc_8x8([5; 8], [5; 8], false, false);
        assert_eq!(out, [128u8; 64]);
    }

    #[test]
    fn chroma_dc_only_top_or_left() {
        // Only top: per quadrant (top4 + 2) >> 2.
        // top all 10 → top4 = 40 → (42) >> 2 = 10.
        let out = predict_chroma_dc_8x8([10; 8], [99; 8], true, false);
        assert_eq!(out, [10u8; 64]);
        let out = predict_chroma_dc_8x8([99; 8], [10; 8], false, true);
        assert_eq!(out, [10u8; 64]);
    }
}
