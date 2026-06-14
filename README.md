# oxideav-svq

Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Round 295 — SVQ3 two-sided transform `M · X · M^T` composition.** Round 295
composes the two single-sided passes landed across rounds 262/272 (`M · X`,
the column pass) and round 290 (`X · M^T`, the row pass) into the full
two-sided transform the wiki spec's
(`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock transform and
dequantization") matrix implies. Two new `const fn` helpers chain the existing
pinned passes — no new matrix or constant is introduced:
`svq3_dequant::apply_luma_transform_2d(block: [i32; 16]) -> [i32; 16]`
(= `apply_luma_transform_columns(apply_luma_transform_rows(block))`, realising
`M · (X · M^T) = M · X · M^T`) and
`svq3_dequant::apply_chroma_dc_2x2_2d(block: [i32; 4]) -> [i32; 4]`
(the 2×2 chroma analogue). The composition order is matrix-algebra
associativity of two already-pinned passes, not an additional spec fact, and
consistent with both single-sided helpers **no inter-pass shift, bias, or
quantiser scaling is applied** — the wiki lists the matrix under "Transform
coefficients" without enumerating any normalisation between passes. New tests
corroborate each helper against a brute-force triple-loop `M · X · M^T`
reference, verify the order-independence (`(M·X)·M^T = M·(X·M^T)`; the chroma
matrix is symmetric), and check linearity and `const`-context evaluability.
`Svq3DecoderHandle::receive_frame` continues to return
`oxideav_core::Error::Unsupported`. Total tests: 420 lib + 7 integration + 10
doc = 437 (up from 411 + 7 + 8 = 426).

**Round 290 — SVQ3 transform row-multiply (`X · M^T`) passes.** Round 290
adds the right-side mirror of rounds 262/272's single-sided column passes:
where those applied the wiki-pinned transform matrix (`docs/video/svq3/wiki/Sorenson_Video_3.wiki`
§"Macroblock transform and dequantization") against the *columns* of a block
(`M · X`), the new helpers apply the *same* matrix against the block's *rows*
(`X · M^T`). Two new `const fn` helpers:
`svq3_dequant::apply_luma_transform_rows(block: [i32; 16]) -> [i32; 16]`
(`out[r*4+c] = X[r,:] · M[c,:]`, reusing `apply_luma_transform_row` with
`matrix_row = LUMA_TRANSFORM_MATRIX[c]`) and
`svq3_dequant::apply_chroma_dc_2x2_rows(block: [i32; 4]) -> [i32; 4]`
(the 2×2 chroma analogue). Each is a single matrix pass involving only the
verbatim-pinned `M`; the full two-sided `M · X · M^T` composition the wiki
does NOT enumerate stays deferred. New tests cross-check the row pass against
the explicit `X · M^T` definition and against the column pass on the
transposed input (`(X·M^T)^T = M·X^T`). `Svq3DecoderHandle::receive_frame`
continues to return `oxideav_core::Error::Unsupported`. Total tests: 411 lib
+ 7 integration + 8 doc = 426 (up from 402 + 7 + 6 = 415).

**Round 282 — SVQ3 4×4 diagonal-down intra predictor.** Round 282
lands the one intra predictor the wiki spec pins completely in
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra prediction" as a
new `svq3_pred` module: the 4×4 diagonal-down quirk, whose fill
picture (`a b c c / b c c c / c c c c / c c c c`) and three
closed-form samples (`a = (left[1] + top[1]) / 2`, `b = (left[2] +
top[2]) / 2`, `c = (left[3] + top[3]) / 2`) are spelled out verbatim
in the local mirror. The per-pair average lands as the `const fn`
`svq3_pred::diagonal_down_sample(left_k, top_k) -> u8` (the spec's
plain integer `/ 2` — no rounding bias; operands are non-negative so
the division is a floor), the fill picture as
`svq3_pred::DIAGONAL_DOWN_PATTERN: [u8; 16]` (each entry selecting
one of the derived `a` / `b` / `c` samples), and the combined block
predictor as `svq3_pred::predict_diagonal_down_4x4(left, top) -> [u8;
16]` (row-major output matching the `svq3_scan` / `svq3_dequant`
block layout, so the eventual predicted+residual writeback can
combine the two element-wise). The spec formulas never reference
element `0` of either neighbour array; the three consumed indices are
surfaced as `svq3_pred::DIAGONAL_DOWN_NEIGHBOUR_INDICES = [1, 2, 3]`.
Round 282 targeted the full intra per-block reconstruction
composition, but every remaining stage is doc-gapped: the 4×4
scan-order arrays (round 233's ambiguity note), the two-sided `M · X
· M^T` transform (rounds 262/272 deferrals), the numeric
mode-to-predictor binding, the other H.264-back-referenced
predictors, and the writeback clamp all await pinning in
`docs/video/svq3/`. Total tests: 402 lib + 7 integration + 6 doc =
415 (up from 387 + 7 + 4 = 398).

**Round 272 — SVQ3 4×4 luma transform application helpers.** Round 272
lands the per-block application of the 4×4 luma transform matrix the wiki
spec pins in `docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock
transform and dequantization" under "Transform coefficients" (already
exposed verbatim as [`svq3_dequant::LUMA_TRANSFORM_MATRIX`]), mirroring
round 262's chroma DC helpers. Two new `const fn` helpers:
`svq3_dequant::apply_luma_transform_row(matrix_row, a, b, c, d) -> i32`
carries out one matrix-row dot product against a 4-point column (every
row shares column-0 `13`, so a pure-DC column `[a,0,0,0]` yields
`13 * a`), and `svq3_dequant::apply_luma_transform_columns(block: [i32;
16]) -> [i32; 16]` applies the matrix against the columns of a row-major
4×4 input block (the `M · X` single-sided pass; `out[r*4+c] = M[r,:] ·
X[:,c]`). The unrounded outputs feed directly into
`svq3_dequant::dequantize_coefficient`. The full two-sided `M · X · M^T`
luma transform the wiki spec does NOT spell out is deliberately NOT
folded in here — that derivation belongs in a future round once docs pin
it. Total tests: 387 lib + 7 integration + 4 doc = 398 (up from 377 + 7
+ 2 = 386).

**Round 262 — SVQ3 2×2 chroma DC transform application helper.** Round
262 lands the per-block application of the 2×2 chroma DC transform
matrix the wiki spec pins in `docs/video/svq3/wiki/Sorenson_Video_3.wiki`
§"Macroblock transform and dequantization" as `[[8, 8], [8, -8]]` (also
exposed verbatim as [`svq3_dequant::CHROMA_DC_TRANSFORM_MATRIX`]). Two
new `const fn` helpers surface the transform's two natural application
forms: `svq3_dequant::apply_chroma_dc_transform_row(matrix_row, a, b) ->
i32` carries out one matrix-row dot product against a 2-point column
(`row 0 · (a, b) = 8 * (a + b)`; `row 1 · (a, b) = 8 * (a - b)`), and
`svq3_dequant::apply_chroma_dc_2x2_columns(block: [i32; 4]) -> [i32; 4]`
applies the full matrix against the columns of a row-major 2×2 input
block (the `M · X` single-sided pass). The input layout matches
`svq3_scan::place_chroma_dc_2x2`'s row-major output (`block[0]` =
`(0,0)`, `block[1]` = `(0,1)`, `block[2]` = `(1,0)`, `block[3]` =
`(1,1)`); the return value is laid out the same way so the result feeds
directly into the subsequent `svq3_dequant::dequantize_chroma_dc` per-
sample dequant step. The full two-sided `M · X · M^T` transform that
the wiki spec does NOT spell out explicitly is deliberately NOT folded
in here — that derivation belongs in a future round once docs pin it.
Total tests: 377 lib + 7 integration + 2 doc = 386 (up from 359 + 7 =
366).

**Round 245 — SVQ3 alt-scan two-half block walker.** Round 245 lands
the typed two-half walker for SVQ3's alternative-scan coefficient block
as a new `svq3_coeff::AltScanBlock` carrier plus a
`svq3_coeff::read_alt_scan_block` entry point. The wiki spec
(`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient decoding")
pins the alt-scan path as "coded in two parts of up two eight
coefficients corresponding to each half-scan": each half is an
independent run of up to `COEFFS_PER_ALT_SCAN_HALF = 8` coefficients,
terminated by either an explicit end-of-block sentinel (`code == 0`) or
by reaching the half's capacity, and the two halves keep their
run-accumulator cursors strictly independent. The `AltScanBlock` carrier
exposes the two halves as separate `Vec<Coefficient>` fields
(`first_half` / `second_half`) for the future placement step that pins
the 4×4 scan-order arrays, plus three convenience accessors: `len()`
returning the cumulative coefficient count bounded by
`COEFFS_PER_4X4_BLOCK = 16`, `iter_with_half()` yielding
`(half_index, Coefficient)` pairs in stream order, and `flatten()`
producing a single `Vec<Coefficient>` for callers that have already
absorbed the half-scan boundary. The walker itself sequences two
`read_alt_scan_half(br)` calls back-to-back, preserving the half
walker's per-half termination + per-half capacity guarantees verbatim.
The per-half 8-position scan-order array the wiki spec depicts in
§"Macroblock layer" remains deferred per round 233's ambiguity note.
Total tests: 359 lib + 7 integration = 366 (up from 346 + 7 = 353).

**Round 242 — SVQ1 per-stage codebook-index field reader (§4.2).**
Round 242 lands the per-stage codebook-index field reader the SVQ1
spec defines in `docs/video/svq1/spec/04-multistage-vq-decoder.md`
§4.2 as a new `svq1_stage_indices` module. The wiki source
(`docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Decoding Intraframe
Plane Data") pins the run as `4 × N` raw bits — `N` consecutive 4-bit
fields in stage-ascending order with no inter-stage padding and no
permutation field ("the first 4 bits specify the first codebook to
use, the next 4 bits specify the second codebook, all the way up to a
sixth codebooks for 6 possible stages"). The
`svq1_stage_indices::read_stage_indices` function reads exactly that
run from a `BitReader` and returns an allocation-free
`svq1_stage_indices::IndexBuffer` of up to
`svq1_stage_indices::MAX_STAGES_PER_LEAF = 6` entries. The per-stage
width `svq1_stage_indices::BITS_PER_INDEX = 4`, the maximum index
`svq1_stage_indices::MAX_VEC_IDX = 15`, and the closed-form bit-count
helper `svq1_stage_indices::bits_for_n_stages` (returning `4 × N` for
`N ∈ 0..=6` and `None` otherwise) surface the spec invariants for
callers that want to range-check or budget bits up front. The reader
honours the `N = 0` mean-only leaf path (spec/04 §4.5.4) by returning
`IndexBuffer::EMPTY` without consuming any bits; `N > 6` is rejected
with `Error::BadBitWidth` (the stage-count VLC alphabet excludes that
case per spec/04 §4.1.1); mid-run end-of-stream surfaces as
`Error::Truncated`. Round 242 is the bitstream-side index reader
alone — the §4.3 codebook lookup
(`(level, half, stage, vec_idx) → V_L bytes`) still depends on the
L=0..L=3 payload's intra-vs-inter / stage-vs-level interleave pinned
in `docs/video/svq1/`. Total tests: 346 lib + 7 integration = 353
(up from 326).

**Round 239 — SVQ1 mean-step saturating arithmetic.** Round 239 lands
the per-sample mean-step apply arithmetic documented in
`docs/video/svq1/spec/05-mean-removal.md` §5.4 as a new `svq1_mean`
module. The two halves of the SVQ1 mean family — intra (`u8 ∈ [0,
255]` per §5.1.1) and inter (`s9 ∈ [-256, +255]` per §5.1.2 / §5.1.3)
— are exposed as the `const fn` helpers `apply_intra_mean_step` and
`apply_inter_mean_step`, with the underlying `saturate_u8` clamp
matching the wiki spec's "saturate to an unsigned byte range, 0..255"
mandate (§5.4.3). A companion `samples_per_leaf` helper returns the
`V_L` replication count per spec/03 §3.3 (`8 / 16 / 32 / 64` for
L=0..L=3; `None` for L=4 / L=5 since those levels do not host a
mean-removed VQ leaf per spec/14.10 / §14.11). Range constants
(`INTRA_MEAN_MIN` / `INTRA_MEAN_MAX` / `INTER_MEAN_MIN` /
`INTER_MEAN_MAX`) pin the two-table value-range distinction at the
type level; a `MeanError::OutOfRange` variant flags out-of-range
inter mean values. Round 239 is pure arithmetic — no bitstream reads
— the future mean-VLC wire-up round will read the intra (alphabet
256, file `0x5cb0c..0x5cf14`) and inter (alphabet 512, `min_value =
-256`, file `0x5c304..0x5cb0c`) mean-VLC tables per spec/05 §5.7 and
feed the decoded mean into these helpers. Total tests: 319 lib + 7
integration = 326 (up from 314).

**Round 233 — SVQ3 per-block coefficient placement (scan-order
infrastructure).** Round 233 lands the per-block placement step that
connects the Golomb-decoded `(run, value)` coefficient stream from
`svq3_coeff` to the 2D block matrix consumed by the dequantization
arithmetic in `svq3_dequant`. The new `svq3_scan` module surfaces an
unambiguous 2×2 chroma DC row-major scan order
(`CHROMA_DC_2X2_SCAN = [0, 1, 2, 3]`) — the wiki spec's §"Coefficient
decoding" notes that "chroma DCs are stored in 2×2 blocks" without
drawing a dezigzag picture for the 2×2 case, and the row-major order
is the only natural one for a 2×2 block in row-major storage. A
generic `place_coefficients_in_scan_order::<DEST_LEN>(coeffs,
scan_order)` helper walks a `(run, value)` stream into a fixed-size
flat array per a scan-order table, advancing a cursor by `run + 1`
per non-zero coefficient (per the wiki spec's §"Coefficient
decoding" semantics). The convenience wrapper
`place_chroma_dc_2x2(coeffs) -> [i32; 4]` pins the chroma DC case so
its output feeds directly into the
`svq3_dequant::CHROMA_DC_TRANSFORM_MATRIX = [[8, 8], [8, -8]]`
application. Round-trip helpers (`chroma_dc_2x2_flat_index`,
`chroma_dc_2x2_matrix_position`) and a typed `ScanError` enum
(`OutOfRange` / `InvalidScanOrderEntry` /
`ScanOrderLengthMismatch`) round out the module. The 4×4 scan-order
arrays (alt-scan picture and normal-zigzag) are **not** transcribed
in round 233: the wiki's §"Macroblock layer" "Dezigzag pattern (from
H.264)" ASCII art has two unresolved characteristics — (a) the
picture's row-0 horizontal arrows depict three adjacent positions
`(0,0)→(0,1)→(0,2)` (matching neither the H.264 frame-zigzag opening
triple `(0,0)→(0,1)→(1,0)` nor the H.264 alt-scan opening triple
`(0,0)→(1,0)→(0,1)`); (b) the wiki text uses "normal zigzag" as the
not-this-picture case without depicting it. Round 233 defers the
4×4 scan-order arrays to a future docs round that pins their
canonical interpretation in `docs/video/svq3/`. Round 233 is
structural arithmetic only — `Svq3DecoderHandle::receive_frame`
continues to return `oxideav_core::Error::Unsupported` until the
4×4 dezigzag + IDCT stages land. Total tests: 307 lib + 7
integration = 314 (up from 290).

**Round 230 — SVQ3 macroblock transform + dequantization arithmetic.**
Round 230 implements the per-coefficient dequantization arithmetic the
wiki spec defines for SVQ3 in
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock transform and
dequantization" as a new `svq3_dequant` module. The 4×4 luma transform
coefficient matrix (`LUMA_TRANSFORM_MATRIX` = `[[13, 17, 1, 7], [13, 7,
-1, -17], [13, -7, -1, 17], [13, -17, 1, -7]]`), the 2×2 chroma DC
transform matrix (`CHROMA_DC_TRANSFORM_MATRIX` = `[[8, 8], [8, -8]]`),
and the 32-entry per-quantiser scale table (`DEQUANT_COEFF_TABLE`,
indexed by the slice quantiser `Q ∈ 0..32`, monotonically increasing
from `3881` at `Q=0` to `141533` at `Q=31`) land verbatim as `pub
const` arrays. The three closed-form dequantization expressions the
wiki spec quotes — intra-luma DC (`13 * 13 * 1538 * block[0]` =
`INTRA_LUMA_DC_SCALE * block[0]` with `INTRA_LUMA_DC_SCALE = 259_922`),
chroma DC (`(DEQUANT_COEFF_TABLE[Q] * (block[0] >> 3)) >> 1`), and
general per-coefficient dequant (`(coeff * DEQUANT_COEFF_TABLE[Q] + dc
+ 0x80000) >> 20`) — land as the `dequantize_intra_luma_dc(block_zero)
-> i32`, `dequantize_chroma_dc(q, block_zero) -> i32`,
`dequantize_coefficient(q, coeff, dc) -> i32`, and `finalise_dc(dc) ->
i32` `const fn` helpers. The dequant shift (`DEQUANT_SHIFT = 20`) and
the round-half-up bias (`DEQUANT_ROUND = 0x80000 = 1 << 19`) are
surfaced as provenance constants. Round 230 is structural arithmetic
only — `Svq3DecoderHandle::receive_frame` continues to return
`oxideav_core::Error::Unsupported` until the dezigzag + IDCT stages
land. Total tests: 283 lib + 7 integration = 290 (up from 250).

**Round 224 — SVQ3 sub-pixel thirdpel interpolation arithmetic.** Round
224 implements the per-sample interpolation arithmetic the wiki spec
defines for SVQ3 motion compensation in
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation" as
a new `svq3_mc` module. The `thirdpel_interpolate_1d(a, b) -> i32`
`const fn` implements the spec's one-direction formula
`((2 * A + B + 1) * 0x2AB) >> 11` (with `0x2AB` / `>> 11` / `+1`
surfaced as `THIRDPEL_1D_MULTIPLIER` / `THIRDPEL_1D_SHIFT` /
`THIRDPEL_1D_BIAS`); the `thirdpel_interpolate_2d(a, b, c, d) -> i32`
`const fn` implements the spec's two-dimensional formula
`((4 * A + 3 * B + 3 * C + 2 * D + 6) * 0xAAB) >> 15` against the
spec-quoted weight matrix `THIRDPEL_2D_WEIGHTS = [[4, 3], [3, 2]]`
(with `0xAAB` / `>> 15` / `+6` surfaced as `THIRDPEL_2D_MULTIPLIER` /
`THIRDPEL_2D_SHIFT` / `THIRDPEL_2D_BIAS`). Both helpers consume the
spec's exact integer arithmetic — no division and no floating point.
Round 224 also surfaces the wiki spec's "stored and predicted as
fraction of six and then rounded to the desired base" motion-vector
storage remark as a per-precision base lookup
(`stored_sixths_base(Svq3MvPrecision::Fullpel) == 6`, `Halfpel == 3`,
`Thirdpel == 2`) plus an `is_aligned_to_precision_base(stored_sixths,
precision) -> bool` predicate. The actual rounding step is NOT yet
implemented — the wiki spec leaves the rounding direction (toward
zero / away from zero / round-half-up / round-half-even) unspecified,
so round 224 exposes only the alignment check, deferring the rounding
helper to the round that pins the direction in `docs/`. Round 224
is structural arithmetic only —
`Svq3DecoderHandle::receive_frame` continues to return
`oxideav_core::Error::Unsupported`. Total tests: 243 lib + 7
integration = 250 (up from 219).

**Round 217 — SVQ1 u16-LE parameter table (512 records) mirrored.**
Round 217 picks up the third Extractor-02-staged helper region from
`docs/video/svq1/tables/`: the 1024-byte `u16` parameter table at
file offset `0x59d00..0x5a100` (VMA `0x67dc9d00..0x67dca100`, section
`.rdata`). The CSV+meta pair (`tables/u16_param_table.{csv,meta}`)
ships bit-exact against `docs/video/svq1/tables/u16_param_table.*`;
both SHA-256s are appended to the local `MANIFEST-02.sha256` and
match the docs-side manifest. The meta classifies the values
("values from `{0x0000, 0x0001, 0x0002, 0x0010, 0x0014, 0x0020,
0x0028, 0x0048, 0x0068, 0x0081, 0x0082, 0x0084, 0x0101, 0x0102,
0x0181, 0x0182}` arranged in groups") and notes the table sits
immediately below the saturating-clip LUT (which begins at
`0x5a100`); both observations are encoded as compile-time and lib
tests below.

`build.rs` gains a `parse_u16_csv` helper (sister of the existing
`parse_unsigned_csv`) that asserts the `word_index` column is
gapless `0..512`, parses the `value_u16` column, and emits
`SVQ1_U16_PARAM_TABLE: [u16; 512]` under
`$OUT_DIR/svq1_codebook_data.rs`. The existing `svq1_helper_luts`
module gains the new constant, a borrowed accessor
(`u16_param_table`), per-table length constants
(`SVQ1_U16_PARAM_TABLE_WORDS = 512`,
`SVQ1_U16_PARAM_TABLE_BYTES = 1024`), source-region provenance
constants (`SVQ1_U16_PARAM_TABLE_FILE_OFFSET = 0x0005_9d00`,
`SVQ1_U16_PARAM_TABLE_VMA = 0x67dc_9d00`), and a typed
`SVQ1_U16_PARAM_TABLE_ALLOWED_VALUES: [u16; 16]` ascending list
that the meta's brace-enumerated set is encoded as. Seven new lib
tests cover the length / structural invariants: documented record
count and byte length per meta; the source-region offsets and
derivable image base `0x67d70000` (VMA − file offset); the flush
geometry that ties the table's exclusive end VMA `0x67dca100` to
`SVQ1_CLIP_LUT_VMA`; the closed-set invariant (every entry belongs
to the documented allowed-value set); the four-word `0x0000`
prelude at `word_index 0..4` (file offsets `0x59d00..0x59d08`); the
first non-zero `0x0020 × 9` run at `word_index 4..13` (file offsets
`0x59d08..0x59d1a`); and a strictly-ascending-uniqueness guard over
the allowed-value set itself. Neither the bytes nor the new
accessor is wired into a pixel decode path yet — the
reconstruction stage that would consume the LUTs remains gated on
the L=0..L=3 intra-vs-inter / stage-vs-level layout doc still
pending for the codebook payload; round 217 makes the bit-exact
constants available so the future pixel work can lift them in
unchanged. Total tests: 212 lib + 7 integration = 219 (up from
212).

**Round 203 — SVQ1 saturating-clip + bit-mask helper LUTs.** Round
203 lands two small helper LUTs the docs collaborator's Extractor 02
pass identified in the SVQ1 `.rdata` region. Both CSV+meta pairs ship
under `crates/oxideav-svq/tables/` as bit-exact mirrors of
`docs/video/svq1/tables/`, their SHA-256s appended to the local
`MANIFEST-02.sha256` and matching the docs-side manifest:

* **Saturating-clip LUT** — 768 bytes at reference-binary file offset
  `0x5a100..0x5a400` (VMA `0x67dca100..0x67dca400`, section `.rdata`,
  `clip_lut.{csv,meta}`). Per the meta: prelude + central wrap-around
  ramp `0x80..0xff, 0x00..0x7f`; used by the codec on interpolation /
  overflow-saturation paths. The meta is explicit that this is NOT a
  VQ codebook.
* **Bit-position / bit-mask LUT** — 16 bytes at file offset
  `0x5c1c4..0x5c1d4` (VMA `0x67dcc1c4..0x67dcc1d4`,
  `svc_bitmask_lut.{csv,meta}`). First 8 entries are the descending
  single-bit masks `0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01`;
  last 8 are their one's complements
  `0x7f, 0xbf, 0xdf, 0xef, 0xf7, 0xfb, 0xfd, 0xfe`.

`build.rs` is extended with an unsigned-CSV parser (`parse_unsigned_csv`)
that asserts `byte_index` is gapless `0..N`, parses the
`value_unsigned` column as `u8`, and emits `SVQ1_CLIP_LUT: [u8; 768]`
+ `SVQ1_BITMASK_LUT: [u8; 16]` under `$OUT_DIR/svq1_codebook_data.rs`.
The new [`svq1_helper_luts`] module exposes the two constants,
borrowed accessors (`clip_lut`, `bitmask_lut`), per-LUT byte-count
constants (`SVQ1_CLIP_LUT_BYTES = 768`, `SVQ1_BITMASK_LUT_BYTES = 16`),
and source-region provenance constants (file offset + VMA pair for
each region: `SVQ1_CLIP_LUT_FILE_OFFSET = 0x0005_a100`,
`SVQ1_CLIP_LUT_VMA = 0x67dc_a100`,
`SVQ1_BITMASK_LUT_FILE_OFFSET = 0x0005_c1c4`,
`SVQ1_BITMASK_LUT_VMA = 0x67dc_c1c4`). Eight new lib tests cover the
length / structural invariants: documented byte-length per meta;
descending bit-mask half (`SVQ1_BITMASK_LUT[i] == 1 << (7 - i)`);
one's-complement half (`SVQ1_BITMASK_LUT[i+8] == !SVQ1_BITMASK_LUT[i]`);
the exact 16-byte bitmask string as quoted in the meta; the clip-LUT
ramp head `0x80..0x8f` at byte offset `0x10..0x20` corroborating the
meta's "characteristic identity-ramp pattern starting at 0x5a118"
remark; and the derivable `image_base = 0x67d70000` consistency for
both regions (VMA − file_offset). Neither LUT is wired into a pixel
decode path yet — the reconstruction path that would consume them
remains gated on the L=0..L=3 intra-vs-inter / stage-vs-level layout
doc still pending for the codebook payload; round 203 makes the
bit-exact constants available so the future pixel work can lift them
in unchanged. Total tests: 205 lib + 7 integration = 212 (up from
204).

**Round 197 — SVQ1 L=4 / L=5 codebook ABSENCE wired end-to-end.**
The docs collaborator's Extractor 02 pass (`docs/video/svq1/spec/
14.10-codebook-L4.md`, `spec/14.11-codebook-L5.md`,
`tables/codebook-l{4,5}.meta`, `provenance/02-codebook-extraction.md`)
RESOLVED the long-standing L=4 / L=5 codebook gap as
**architecturally absent**: no L=4 (16×8) or L=5 (16×16) codebook
exists in the Sorenson Video TM for QT R2.0 build; both block sizes
are always subdivided to ≤8×8 before quantisation. Round 197
mirrors the two new `codebook-l{4,5}.meta` records bit-exact under
`crates/oxideav-svq/tables/`, extends `build.rs` to parse them at
build time (asserting `status: ABSENT` + matching the canonical
per-vector / per-half byte counts against the per-level invariants),
and exposes the result as a typed [`Svq1AbsentLevelRecord`] +
[`SVQ1_L4_ABSENCE`] / [`SVQ1_L5_ABSENCE`] constants alongside a
new [`Svq1Level::absence_record`] `const fn` accessor that returns
`Some(SVQ1_L4_ABSENCE)` / `Some(SVQ1_L5_ABSENCE)` for the L=4 / L=5
absent levels and `None` for the L=0..L=3 present-codebook levels.
A new `tests/svq1_codebook_l4_l5_absence.rs` integration suite
walks every crate-public surface that depends on the absence
contract — `Svq1Level::codebook_bytes_per_half`, the new
`absence_record` accessor, the `SVQ1_L{4,5}_ABSENCE` constants
themselves, the `read_block_decision` walker's
`Error::InvalidLevelQuantise` rejection path, and the 16-entry
block-shape LUT that caps quantised block sizes at L=3 — and asserts
they all agree on which levels are absent (L=4, L=5) versus present
(L=0..L=3). Total tests: 197 lib + 7 integration = 204 (up from 192).
The internal intra-vs-inter ordering and stage-vs-level interleave
within the 23004-byte L=0..L=3 payload remains a sibling docs spec
task — full pixel reconstruction still waits on that layout doc.

Carried forward from round 9: **SVQ1 L=0..L=3 codebook payload.** The 23004-byte
mean-removed multistage VQ payload and its 36-byte
descriptor/block-shape prefix are now compile-time constants in the
new `svq1_codebook` module. The bytes come bit-exact from
`tables/codebook-l0l3.csv` + `tables/codebook-descriptor.csv`,
mirrors of `docs/video/svq1/tables/` populated by the docs-collab
Extractor 02 pass (`docs/video/svq1/provenance/02-codebook-extraction.md`)
from the reference binary `quicktimethirdparty.qtx`
SHA-256 `ac3509bf22aa1458dfc6e1af980956c0153b4c287af452ae5b9cac6f923be169`,
file offset `0x5d200..0x62c00` (VMA `0x67dcd200..0x67dd2c00`). A new
build script parses the CSVs at build time and emits
`SVQ1_CODEBOOK_L0L3_BYTES: [i8; 23004]`,
`SVQ1_CODEBOOK_DESCRIPTOR: [u8; 36]`, and
`SVQ1_BLOCK_SHAPE_LUT: [u8; 16]` under `$OUT_DIR/`; ergonomic
accessors (`codebook_l0l3_payload`, `codebook_descriptor`,
`block_shape_lut`) plus per-level size constants
(`SVQ1_CODEBOOK_PAYLOAD_BYTES = 23004`,
`SVQ1_CODEBOOK_DESCRIPTOR_BYTES = 36`,
`SVQ1_STAGES_PER_LEVEL = 6`,
`SVQ1_ENTRIES_PER_STAGE = 16`) round out the surface. A new
`Svq1Level::codebook_bytes_per_half()` const method returns the
per-level codebook size for one half (intra OR inter) — 768 / 1536
/ 3072 / 6144 for L=0..L=3, `None` for L=4 / L=5 — matching the
`docs/video/svq1/tables/codebook-l0l3.meta` size arithmetic
`2 × (768 + 1536 + 3072 + 6144) = 23040 B = 36 B descriptor +
23004 B payload`. Eleven new unit tests cover payload + descriptor
lengths, the full-region size arithmetic, per-level byte counts, the
L=4 / L=5 `None` rejection, the 16-entry block-shape LUT against the
exact byte string recorded in `codebook-descriptor.meta` line 22
(`04 04 03 02 04 03 03 02 03 03 02 02 03 02 02 01`), the LUT cap at
`1..=4` (corroborating the §14.10 / §14.11 ABSENT findings), the
first descriptor record's `(b0=0x03, b3=0x18, b4=0x02)` byte
pattern, the first 16 i8 entries against `codebook-l0l3.hex` row 1
(`02 01 00 ff 01 00 ff ff 01 00 ff fe 00 ff fe fd`), and accessor
aliasing. Total tests: 192 (up from 181). Round 9 deliberately does
NOT yet expose a `(level, stage, intra_or_inter, vector_idx) →
&[i8]` lookup — the precise intra-vs-inter ordering and
stage-vs-level interleave WITHIN the 23004-byte payload is a sibling
docs spec task per `codebook-l0l3.meta` lines 30-32 ("the L0..L3
spec's concern"). Full pixel reconstruction unblocks when the
internal-layout spec lands.

Carried forward from round 8: **SVQ1 block-tree subdivision walker
(structural).** The recursive subdivide-vs-quantise decision tree
the wiki spec defines in §"Decoding Intraframe Plane Data" lands as
the `svq1_blocktree` module. The walker covers all six levels (L=5
16×16 down to L=0 4×2), reads one bit per non-leaf decision (with
L=0 short-circuiting to "quantise"), and surfaces the wiki spec's
`(stages > 0) && (level >= 4)` invalid-vector branch through the
`Error::InvalidLevelQuantise(Svq1Level)` variant. The L=4 / L=5
rejection is corroborated by `docs/video/svq1/spec/14.10-codebook-L4.md`
and `docs/video/svq1/spec/14.11-codebook-L5.md` (both resolve to
"no codebook stored at this level in this build — always
subdivided"). The round-8 public surface is `Svq1Level { L0..L5 }`
(with `block_dims` / `vector_length` /
`rejects_in_place_quantise` const accessors),
`Svq1BlockDecision { Subdivide, Quantise }`,
`read_block_decision(level, &mut BitReader)`, and a `const fn
subdivide(level)` returning the two child levels.

Carried forward from round 7: **SVQ3 intra-4×4
predictor-from-neighbour resolution helper.** The wiki spec's
§"Intra macroblock information decoding"
describes the per-sub-block 4×4 intra-prediction mode as a lookup
`pred_table[top + 1][left + 1][idx]` against the
[`svq3_mb::INTRA_PRED_TABLE`] constant, with two substitution rules
folded into the surrounding prose: "when predictors lie outside of
slice, `-1` is used instead" and "for 16×16 intra and any inter
blocks value of `2` is used as the predictor". Round 7 lands the
substitution rules as a typed neighbour-classification enum
(`svq3_mb::IntraNeighbour::{Outside, Intra16x16OrInter, Mode4x4(u8)}`)
and a `resolve_intra_4x4_predictor(top, left, idx)` helper that
performs the lookup, honours both substitution rules, and surfaces the
spec's "if table value is -1 then input data was incorrect" condition
as a new `Error::InvalidIntraPrediction(top, left, idx)` variant. A
companion `resolve_intra_4x4_pair(top, left, (a, b))` walks both
elements of an [`svq3_mb::INTRA_PRED_PAIRS`] entry against the same
neighbour context and returns the resolved `(top_mode, left_mode)`
tuple the per-sub-block intra-prediction stage will consume. Round 7
remains structural — the per-sub-block intra-prediction VLC and the
actual pixel-domain intra-prediction stage are still out of scope.

Carried forward from round 6: **SVQ3 inter-MB motion-vector
precision selector.** The
three-branch decision documented in the wiki spec's §"Inter
macroblock information decoding" lands in the `svq3_mb` module as a
typed reader returning one of three sample-grid precisions
(`Svq3MvPrecision::{Fullpel, Halfpel, Thirdpel}`). The selector
honours the spec's short-circuit evaluation exactly: when the
sequence header's `has_thirdpel` / `has_halfpel` flags are both off
no bit is consumed and the result is `Fullpel`; when exactly one
flag is set the selector reads one bit; when both are set the
function reads one or two bits depending on the first bit's value.
The frame-type-dispatched `read_inter_mv_precision` also
short-circuits B-frame inter macroblocks to halfpel without reading
any bit, per the spec's §"Macroblock transform and dequantization"
remark "it is always halfpel precision in B-frames". Round 6 remains
structural — the actual motion-vector difference VLC is still gated
on the spec's SVQ3 MV component VLC table, which is not enumerated
bit-for-bit in the local mirror.

Carried forward from round 5: **SVQ3 residual coefficient walker.**
The per-block Golomb-coded `(run, value)` residual coefficient
stream from the wiki spec's §"Coefficient decoding" lands in the
`svq3_coeff` module: three coefficient-table variants (2×2 chroma
DC, alternative-scan 4×4 luma-intra-with-low-quantiser, normal-
zigzag everything else) plus the two run-correction arrays
(`INTRA_RUN_CORRECTION`, `INTER_RUN_CORRECTION`). Each per-table
single-coefficient reader (`read_chroma_dc_coefficient` /
`read_alt_scan_coefficient` / `read_normal_scan_coefficient`)
decodes one Golomb code + sign bit, returning `Ok(None)` on
end-of-block. The block-level walkers (`read_chroma_dc_block` /
`read_alt_scan_half` / `read_normal_scan_block`) loop until the
end-of-block sentinel is seen or the per-block coefficient cap is
reached; structural overflow surfaces as `Error::BadBitWidth`.
Round 5 remains structural — de-zigzag, dequantisation, and IDCT
are not yet wired; `Svq3DecoderHandle::receive_frame` continues to
return `oxideav_core::Error::Unsupported`.

Carried forward from round 4: **SVQ3 macroblock-type tree walk
(structural).** `read_mb_type(&mut BitReader, Svq3FrameType)`
walks a single `ue(v)` exp-Golomb code at the bit-reader cursor
and returns a typed `Svq3MbType` (I-frame intra / P-frame inter /
P-frame intra / B-frame inter / B-frame intra). The 25-entry
intra-mode pair table, the 6×6×5 intra-mode context-lookup table,
and the 4×4 intra sub-block scan order from §"Intra macroblock
information decoding" land as `pub const` arrays alongside the
walker.

Carried forward from round 3: **SVQ3 SEQH + slice-header parser
(structural).** `SEQH` extradata yields a typed
`Svq3SequenceHeader` (frame-size code, dimensions, halfpel /
thirdpel-precision flags, no-B-frames flag, optional-byte trailer,
protected flag) and each on-wire slice yields a typed
`Svq3SliceHeader` (version, slice-size, frame type, frame number,
slice quantiser, …) after the wiki spec's byte-permutation is
reversed. The `SVQ3` FourCC is wired into
[`oxideav_core::CodecRegistry`](https://docs.rs/oxideav-core)
alongside SVQ1.

Carried forward from round 2: **`oxideav-core` framework
integration** for SVQ1. The round-1 structural SVQ1 frame-header
parser is wired into the framework registry via a default-on
`registry` cargo feature: the crate installs a SVQ1 codec entry in
[`oxideav_core::CodecRegistry`](https://docs.rs/oxideav-core) under
the FourCC tags enumerated by
[`docs/video/svq1/wiki/Sorenson_Video_1.wiki`](https://github.com/OxideAV/docs/blob/master/video/svq1/wiki/Sorenson_Video_1.wiki)
line 9 (`svq1` / `SVQ1` / `svqi`) and exposes an
`oxideav_core::Decoder` implementation whose `send_packet` parses the
frame header and whose `receive_frame` returns
`oxideav_core::Error::Unsupported` for the actual pixel-data decode.

What round 2 adds on top of round 1:

* Default-on `registry` cargo feature gating the `oxideav-core`
  dependency. Image / pipeline consumers that only want the frame-
  header parser can depend with `default-features = false` and skip
  the framework dependency tree.
* `oxideav_core::Decoder` implementation (`Svq1DecoderHandle`):
  `send_packet` parses the frame header eagerly (the bitstream's
  structural failure modes surface at `send_packet` rather than later
  at `receive_frame`); the parsed `Svq1FrameHeader` is exposed via
  `Svq1DecoderHandle::last_header()`. `receive_frame` returns
  `Error::Unsupported` until the codebook docs-gap closes.
* Disambiguating `probe_svq1` registered alongside the FourCC tags.
  Returns `1.0` on a structurally valid header, `0.5` on a header
  that's plausibly SVQ1 but truncated, `0.5` when no packet is
  available (FourCC alone is highly disambiguating), `0.0` on a
  structurally invalid header.
* `register(&mut RuntimeContext)` / `register_codecs(&mut
  CodecRegistry)` / `make_decoder(&CodecParameters)` entry points
  plus the `__oxideav_entry` symbol the `oxideav_core::register!`
  macro expands to (so `oxideav-meta`'s build.rs picks the crate up
  automatically).
* `From<crate::Error> for oxideav_core::Error` conversion mapping
  every structural failure to `InvalidData(msg)` with a descriptive
  string; `NotImplemented` maps to `Unsupported`.

What round 2 covers (carried from round 1):

* The 22-bit frame-code field plus the `(frame_code & 0x60) == 0`
  invalid-stream check.
* The 8-bit temporal-reference counter.
* The 2-bit picture-type field, decoded to a `Svq1PictureType` enum
  (`Intra` / `Predicted` / `Droppable`); value `3` is rejected per
  the spec.
* The I-frame trailer chain: optional 16-bit checksum when the frame
  code is `0x50` or `0x60`, optional XOR-obfuscated embedded ASCII
  string when `frame_code ^ 0x10 > 0x50`, the 2 + 2 + 1 "unknown"
  sub-fields, the 3-bit frame-size code, and the explicit 12 + 12
  width/height escape for code `7`.
* The 1-bit checksum-present flag and its `use_packet_checksum:1 +
  component_checksums:1 + reserved2:2` (reserved2 must be 0) trailer.
* The 1-bit `unknown_flag_1` flag, its 1 + 4 + 1 + 2 fixed-width
  sub-fields, and the variable-length `while next-bit-is-1 { read a
  byte }` tail.
* A constant `FRAME_SIZE_TABLE` for the seven well-known dimension
  pairs (160×120 / 128×96 / 176×144 / 352×288 / 704×576 / 240×180 /
  320×240) and the FourCC list (`svq1`, `SVQ1`, `svqi`).
* Four documented structural error conditions plus the truncation
  guard.

What round 5 adds on top of round 4:

* The `svq3_coeff` module: three per-table single-coefficient
  readers (`read_chroma_dc_coefficient` /
  `read_alt_scan_coefficient` / `read_normal_scan_coefficient`) +
  three block-level walkers (`read_chroma_dc_block` /
  `read_alt_scan_half` / `read_normal_scan_block`) + the
  `Coefficient { run, value }` typed result struct.
* `INTRA_RUN_CORRECTION: [i32; 8]` and `INTER_RUN_CORRECTION:
  [i32; 17]` — the two run-correction arrays from the wiki spec
  landed verbatim. Tail handling (`[minus ones]` for alt-scan,
  `[zeroes]` for normal-scan) is implemented by the per-table
  extension formulas.
* `ALT_SCAN_TABLE_0_15: [(u32, i32); 16]` and
  `NORMAL_SCAN_TABLE_0_15: [(u32, i32); 16]` — verbatim
  transcriptions of the wiki spec's first 16 codes for each table.
* Block-capacity constants: `COEFFS_PER_4X4_BLOCK = 16`,
  `COEFFS_PER_CHROMA_DC_BLOCK = 4`, `COEFFS_PER_ALT_SCAN_HALF =
  8`.
* +36 tests covering single-coefficient table lookups (explicit
  codes + closed-form extensions), sign-bit application,
  end-of-block sentinel detection, block-walker capacity caps,
  run-overflow rejection, and truncation propagation (105 → 141
  total).

What round 4 adds on top of round 3:

* The `svq3_mb` module: `read_mb_type` /
  `classify_mb_type` walker + the typed `Svq3MbType` /
  `IFrameMbType` / `PFrameInterMode` / `BFrameInterMode` enums and
  the `is_intra` / `is_inter` / `is_skip` / `num_motion_vectors` /
  `intra` predicate helpers.
* `INTRA_PRED_PAIRS` (`[(u8, u8); 25]`), `INTRA_PRED_TABLE`
  (`[[[i8; 5]; 6]; 6]`), `INTRA_4X4_SCAN_ORDER` (`[u8; 16]`) — the
  three fixed tables from §"Intra macroblock information decoding"
  landed verbatim for use by a future intra-prediction stage.
* Per-slice constants: `I_FRAME_MB_TYPE_MAX = 25`,
  `P_FRAME_MB_TYPE_MAX = 33`, `B_FRAME_MB_TYPE_MAX = 29`,
  `P_FRAME_INTRA_OFFSET = 8`, `B_FRAME_INTRA_OFFSET = 4`.
* +24 tests covering exhaustive code-table classification, Golomb
  decode round-trips, MV-count predicates, table-shape invariants
  (81 → 105 total).

What round 3 adds on top of round 2:

* The `svq3` module: typed `Svq3SequenceHeader` / `Svq3SliceHeader`
  + the `parse_extradata` / `parse_sequence_header` /
  `parse_slice_header` / `parse_wire_slice` /
  `unpermute_slice_payload` / `read_ue_golomb` /
  `num_macroblocks` API.
* `SVQ3_SEQH_MAGIC` (`"SEQH"`) and `SVQ3_FRAME_END` (`0xFF`)
  constants and the `SVQ3_FOURCC_CODES` / `SVQ3_CODEC_ID_STR`
  framework hooks.
* SVQ3 `SVQ3` FourCC registered in the framework registry alongside
  SVQ1 with a separate codec id (`svq3`), a `probe_svq3` probe that
  checks the first-byte version + size-size invariants, and a
  `Svq3DecoderHandle` whose `send_packet` parses the slice (or
  accepts the `0xFF` frame-end sentinel) and whose `receive_frame`
  returns `Error::Unsupported`.
* +29 tests covering the new SEQH + slice-header + permutation +
  Golomb paths (52 → 81 total).

What round 5 still does **not** cover:

* CBP coding. The wiki spec back-references the H.264 CBP table for
  4×4-predicted blocks; the lookup is not enumerated bit-for-bit in
  the local SVQ3 mirror.
* The intra-mode-pair Golomb walk that consumes
  [`svq3_mb::INTRA_PRED_PAIRS`]. The pair → mode lookup is landed,
  but the per-MB Golomb-code-to-pair-index reader is not yet wired.
* Per-partition motion-vector decoding. The wiki spec describes the
  precision-selector + signed-VLC layout for the MV component
  differences but the underlying VLC table is not enumerated
  bit-for-bit in `docs/`.
* De-zigzag of the decoded coefficient stream into the 4×4
  transform-coefficient matrix. The scan order is documented in the
  wiki spec but the placement step is left for the round that wires
  the walker output to the IDCT input.
* Dequantisation + IDCT. The wiki spec gives `svq3_dequant_coeff[Q]`
  and the dequant formula in §"Macroblock transform and
  dequantization"; both land in a later round alongside the IDCT.
* The SVQ3 macroblock-layer decode beyond the residual coefficient
  walker — motion compensation, intra prediction, dequantisation.
  Tracked for future rounds.
* The SVQ3 protected-stream watermark sub-record. The
  variable-length-code encoding of the watermark width / height /
  unknown fields and the role of the deflated watermark image as
  decryption input are out of round-3 scope. Captured at a
  structural level via `Svq3SequenceHeader::protected`.
* The SVQ3 "further scrambled" slice descramble step (separate from
  the documented byte-permutation, which round 3 *does* implement).
  Not yet pinned in `docs/`.

What round 3 still does **not** cover (carried from round 2):

* The encoded SVQ1 plane data (`Y` / `U` / `V`). Decoding those
  requires the SVQ1 multi-stage VQ codebooks and per-level VLC
  tables enumerated in the spec's "Appendix A: SVQ1 Data Tables" —
  still blocked on the docs-collaborator task tracking the codebook
  byte-list extraction. `Svq1DecoderHandle::receive_frame` returns
  `Error::Unsupported` until that lands.
* Embedded-string de-obfuscation. The parser captures the raw
  obfuscated bytes plus the declared length; de-obfuscation is
  deferred until the `string_xor_table[]` is pinned in `docs/`.
* Checksum verification. The spec itself notes "The specific details
  of the checksum coding are not all known"; the value is captured
  for future verification once the algorithm is documented.

## Cargo feature surface

```toml
[dependencies]
# Default: framework-integrated. Pulls in oxideav-core and installs
# the SVQ1 codec into the framework registry.
oxideav-svq = "0.0"

# Standalone: just the SVQ1 frame-header parser, no oxideav-core
# dependency. Suitable for consumers that need to peek at SVQ1
# dimensions / picture-type without bringing in the framework.
oxideav-svq = { version = "0.0", default-features = false }
```

The crate-level standalone API (always available):

* `parse_frame_header(bytes: &[u8]) -> Result<Svq1FrameHeader>`
* `Svq1FrameHeader { frame_code, temporal_reference, picture_type,
  checksum, embedded_string, width, height, frame_size_code,
  checksum_trailer, unknown_flag_1_payload, unknown_flag_1_extras,
  header_end_bit, … }`
* `BitReader::new(&bytes).read_bits(n)` / `read_bit()` / `peek_bit()`
* `FRAME_SIZE_TABLE` + `SVQ1_FOURCC_CODES` + `Svq1PictureType`
* The `Error` enum + `Result<T>` alias.

The framework-integrated surface (when `registry` is on):

* `register(&mut RuntimeContext)` — installs SVQ1 + SVQ3.
* `register_codecs(&mut CodecRegistry)` — same, against a registry
  directly.
* `make_decoder(&CodecParameters) -> Result<Box<dyn Decoder>>`
  (SVQ1) / `make_svq3_decoder(&CodecParameters) -> Result<Box<dyn
  Decoder>>` (SVQ3).
* `probe_svq1(&ProbeContext) -> Confidence` /
  `probe_svq3(&ProbeContext) -> Confidence`.
* `Svq1DecoderHandle::last_header()` /
  `Svq3DecoderHandle::sequence_header()` +
  `Svq3DecoderHandle::last_slice_header()`.
* `From<crate::Error> for oxideav_core::Error`.

The standalone `svq3` module surface (always available):

* `svq3::parse_extradata(&[u8])` /
  `svq3::strip_seqh_prefix(&[u8])` /
  `svq3::parse_sequence_header(&[u8])`.
* `svq3::parse_wire_slice(&[u8], num_mbs, protected)` /
  `svq3::parse_slice_header(...)` /
  `svq3::unpermute_slice_payload(body, slice_size_size)`.
* `svq3::read_ue_golomb(&mut BitReader)`.
* `svq3::num_macroblocks(&Svq3SequenceHeader)`.
* `svq3::Svq3SequenceHeader` / `svq3::Svq3SliceHeader` structs,
  `svq3::SliceVersion` / `svq3::Svq3FrameType` enums.
* `svq3::SVQ3_SEQH_MAGIC` / `svq3::SVQ3_FRAME_END` constants.
* `SVQ3_FOURCC_CODES` / `SVQ3_CODEC_ID_STR` re-exports at the
  crate root.

The standalone `svq3_mb` module surface (always available):

* `svq3_mb::read_mb_type(&mut BitReader, Svq3FrameType) ->
  Result<Svq3MbType>` /
  `svq3_mb::classify_mb_type(Svq3FrameType, u32) ->
  Result<Svq3MbType>` — the macroblock-type Golomb walker + the
  pre-decoded-code classifier.
* `Svq3MbType` / `IFrameMbType` / `PFrameInterMode` /
  `BFrameInterMode` enums + the predicate helpers `is_intra` /
  `is_inter` / `is_skip` / `num_motion_vectors` / `intra`.
* `svq3_mb::INTRA_PRED_PAIRS` (`[(u8, u8); 25]`) /
  `svq3_mb::INTRA_PRED_TABLE` (`[[[i8; 5]; 6]; 6]`) /
  `svq3_mb::INTRA_4X4_SCAN_ORDER` (`[u8; 16]`) — the three fixed
  tables from §"Intra macroblock information decoding".
* `svq3_mb::IntraNeighbour { Outside, Intra16x16OrInter, Mode4x4(u8) }`
  — typed classification of the neighbour macroblock / sub-block whose
  previously-decoded intra-prediction mode feeds the per-sub-block
  predictor lookup. Includes the `lookup_index() -> Result<u8>` helper
  that performs the spec's `-1 → 0` (outside) / `2 → 3` (16×16-intra
  or inter) substitution.
* `svq3_mb::resolve_intra_4x4_predictor(top, left, idx) -> Result<u8>`
  — performs the wiki spec's
  `pred_table[top + 1][left + 1][idx]` lookup, surfacing the `-1`
  sentinel as `Error::InvalidIntraPrediction(top_idx, left_idx, idx)`.
* `svq3_mb::resolve_intra_4x4_pair(top, left, (a, b)) ->
  Result<(u8, u8)>` — walks both elements of an `INTRA_PRED_PAIRS`
  entry against the same neighbour context and returns the resolved
  `(top_mode, left_mode)` tuple.
* `svq3_mb::I_FRAME_MB_TYPE_MAX` / `P_FRAME_MB_TYPE_MAX` /
  `B_FRAME_MB_TYPE_MAX` / `P_FRAME_INTRA_OFFSET` /
  `B_FRAME_INTRA_OFFSET` constants.

The standalone `svq3_coeff` module surface (always available):

* `svq3_coeff::read_chroma_dc_coefficient(&mut BitReader) ->
  Result<Option<Coefficient>>` /
  `svq3_coeff::read_alt_scan_coefficient(...)` /
  `svq3_coeff::read_normal_scan_coefficient(...)` — the three
  per-table single-coefficient readers.
* `svq3_coeff::read_chroma_dc_block(...)` /
  `svq3_coeff::read_alt_scan_half(...)` /
  `svq3_coeff::read_normal_scan_block(...)` — the three block-level
  walkers that gather coefficient triples up to the per-block
  capacity.
* `Coefficient { run: u32, value: i32 }` typed result struct.
* `svq3_coeff::INTRA_RUN_CORRECTION` (`[i32; 8]`) /
  `svq3_coeff::INTER_RUN_CORRECTION` (`[i32; 17]`) /
  `svq3_coeff::ALT_SCAN_TABLE_0_15` /
  `svq3_coeff::NORMAL_SCAN_TABLE_0_15` — the two run-correction
  arrays and the two 16-entry explicit lookups from
  §"Coefficient decoding".
* `svq3_coeff::COEFFS_PER_4X4_BLOCK` /
  `svq3_coeff::COEFFS_PER_CHROMA_DC_BLOCK` /
  `svq3_coeff::COEFFS_PER_ALT_SCAN_HALF` constants.

The standalone `svq3_dequant` module surface (always available):

* `svq3_dequant::LUMA_TRANSFORM_MATRIX: [[i32; 4]; 4]` — the 4×4
  luma transform coefficient matrix
  `[[13, 17, 1, 7], [13, 7, -1, -17], [13, -7, -1, 17], [13, -17, 1, -7]]`.
* `svq3_dequant::LUMA_TRANSFORM_DC_COLUMN = 13` — corroborates the
  invariant that all four rows share `13` in column 0.
* `svq3_dequant::CHROMA_DC_TRANSFORM_MATRIX: [[i32; 2]; 2]` — the 2×2
  chroma DC transform matrix `[[8, 8], [8, -8]]`.
* `svq3_dequant::DEQUANT_COEFF_TABLE: [u32; 32]` — the 32-entry
  per-quantiser scale table, verbatim from the wiki spec.
* `svq3_dequant::DEQUANT_COEFF_TABLE_LEN = 32` /
  `DEQUANT_QUANTISER_RANGE = 0..32` — length and quantiser-range
  constants.
* `svq3_dequant::INTRA_LUMA_DC_SCALE = 259_922` — the `13 * 13 *
  1538` intra-luma-DC scale; also `INTRA_LUMA_DC_SCALE_TAIL = 1538`
  for the standalone scale factor.
* `svq3_dequant::DEQUANT_SHIFT = 20` / `DEQUANT_ROUND = 0x80000`
  — the general dequant's right-shift and round-half-up bias.
* `svq3_dequant::CHROMA_DC_PRE_SHIFT = 3` /
  `CHROMA_DC_POST_SHIFT = 1` — the chroma DC pre / post shifts that
  total `>> 4`.
* `svq3_dequant::dequantize_intra_luma_dc(block_zero) -> i32`
  `const fn` — applies `INTRA_LUMA_DC_SCALE * block_zero`.
* `svq3_dequant::dequantize_chroma_dc(q, block_zero) -> i32`
  `const fn` — applies `(DEQUANT_COEFF_TABLE[q] * (block_zero >>
  3)) >> 1`.
* `svq3_dequant::dequantize_coefficient(q, coeff, dc) -> i32`
  `const fn` — applies `(coeff * DEQUANT_COEFF_TABLE[q] + dc +
  0x80000) >> 20`.
* `svq3_dequant::finalise_dc(dc) -> i32` `const fn` — applies
  `(dc + 0x80000) >> 20` for the no-AC-contribution case.

The standalone `svq3_mc` module surface (always available):

* `svq3_mc::thirdpel_interpolate_1d(a: i32, b: i32) -> i32` `const fn` —
  the one-direction interpolation formula
  `((2 * A + B + 1) * 0x2AB) >> 11`.
* `svq3_mc::thirdpel_interpolate_2d(a: i32, b: i32, c: i32, d: i32) ->
  i32` `const fn` — the two-dimensional interpolation formula
  `((4 * A + 3 * B + 3 * C + 2 * D + 6) * 0xAAB) >> 15`.
* `svq3_mc::THIRDPEL_1D_MULTIPLIER = 0x2AB` (683) /
  `svq3_mc::THIRDPEL_1D_SHIFT = 11` / `svq3_mc::THIRDPEL_1D_BIAS = 1` /
  `svq3_mc::THIRDPEL_1D_WEIGHT_SUM = 3` provenance constants.
* `svq3_mc::THIRDPEL_2D_MULTIPLIER = 0xAAB` (2731) /
  `svq3_mc::THIRDPEL_2D_SHIFT = 15` / `svq3_mc::THIRDPEL_2D_BIAS = 6` /
  `svq3_mc::THIRDPEL_2D_WEIGHT_SUM = 12` provenance constants.
* `svq3_mc::THIRDPEL_2D_WEIGHTS: [[u8; 2]; 2] = [[4, 3], [3, 2]]` — the
  verbatim 2×2 weight matrix the wiki spec quotes.
* `svq3_mc::stored_sixths_base(Svq3MvPrecision) -> u32` `const fn` —
  returns 6 / 3 / 2 for Fullpel / Halfpel / Thirdpel per the wiki spec's
  "stored and predicted as fraction of six and then rounded to the
  desired base" remark.
* `svq3_mc::is_aligned_to_precision_base(stored_sixths: i32,
  Svq3MvPrecision) -> bool` `const fn` — checks whether an
  already-rounded sixths-grid value is on the precision's base.

The standalone `svq3_pred` module surface (always available):

* `svq3_pred::diagonal_down_sample(left_k: u8, top_k: u8) -> u8`
  `const fn` — one diagonal-down predicted sample, the wiki spec's
  `(left[k] + top[k]) / 2` closed form.
* `svq3_pred::predict_diagonal_down_4x4(left: [u8; 4], top: [u8; 4])
  -> [u8; 16]` `const fn` — the full 4×4 diagonal-down predictor;
  row-major output.
* `svq3_pred::DIAGONAL_DOWN_PATTERN: [u8; 16]` — the spec's fill
  picture flattened row-major (`0` ⇒ `a`, `1` ⇒ `b`, `2` ⇒ `c`).
* `svq3_pred::DIAGONAL_DOWN_NEIGHBOUR_INDICES: [usize; 3] = [1, 2,
  3]` — the neighbour-array indices the three closed forms consume.
* `svq3_pred::PRED_4X4_DIM = 4` / `svq3_pred::PRED_4X4_SAMPLES = 16`
  — block-geometry constants.

## Clean-room provenance

Round 282 was implemented strictly from
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra prediction" —
the quirk paragraph beginning "4x4 diagonal down prediction is
performed as", comprising the 4×4 fill picture and the three
closed-form sample equations, all transcribed verbatim. The section's
back-references to H.264 for the remaining predictors were
specifically NOT chased into any other document; those predictors
stay unimplemented until `docs/video/svq3/` carries their sample
equations. No other source was consulted.

Round 230 was implemented strictly from
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock transform and
dequantization" — the four-line block enumerating the 4×4 luma
transform matrix, the two-line block enumerating the 2×2 chroma DC
transform matrix, the 32-entry quantizer table block (lines beginning
`3881, 4351, ...` through `...126635, 141533`), and the three
closed-form expressions for intra-luma DC, chroma DC, and the general
per-coefficient dequant. The constants (`INTRA_LUMA_DC_SCALE` = `13 *
13 * 1538` = `259_922`, `DEQUANT_SHIFT` = `20`, `DEQUANT_ROUND` =
`0x80000`, `CHROMA_DC_PRE_SHIFT` = `3`, `CHROMA_DC_POST_SHIFT` = `1`)
are transcribed verbatim from the same paragraph. No other source was
consulted; the spec paragraph's parenthetical attribution to an
external project file was specifically NOT consulted nor reproduced.

Round 224 was implemented strictly from
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Motion Compensation" —
the four-line paragraph beginning "Thirdpel interpolation in one
direction uses formula" and the preceding sentence on motion-vector
storage ("as fraction of six and then rounded to the desired base").
The interpolation multipliers (`0x2AB`, `0xAAB`), shifts (`11`, `15`),
biases (`+1`, `+6`), and the 2×2 weight matrix `[4 3 / 3 2]` are
transcribed verbatim from that paragraph. The motion-vector-storage
base lookup (Fullpel → 6 / Halfpel → 3 / Thirdpel → 2) follows
directly from the spec's "fraction of six" phrasing combined with the
three precisions enumerated in `Svq3MvPrecision`. No other source was
consulted; in particular the §"Macroblock transform and
dequantization" section (which the round 224 brief flagged as pending
a docs scrub) was not read.

Round 7 was implemented strictly from
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Intra macroblock
information decoding" — specifically the paragraph beginning "Each
element of the pair is then used as an index in the prediction table"
which spells out the `pred_table[top + 1][left + 1][idx]` lookup
shape, the two substitution rules ("when predictors lie outside of
slice, -1 is used instead", "for 16x16 intra and any inter blocks
value of 2 is used as the predictor"), and the error condition
("if table value is -1 then input data was incorrect or intra modes
were predicted incorrectly"). All three rules land as direct
enumerated branches in the new `IntraNeighbour::lookup_index` /
`resolve_intra_4x4_predictor` functions; no other source was
consulted.

Round 5 was implemented strictly from
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Coefficient decoding"
of the same local mirror the prior SVQ3 rounds drew from. The three
coefficient-table variants and both run-correction arrays are
transcribed verbatim from the wiki spec's tables; the closed-form
extension formulas for codes `>= 16` mirror the wiki spec's `code &
0x7` / `code >> 3` / `code & 0xF` / `code >> 4` expressions exactly.

Round 4 was implemented strictly from
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Macroblock layer" /
§"Intra macroblock information decoding" of the same local mirror
the round-3 work drew from.

Round 3 was implemented strictly from
`docs/video/svq3/wiki/Sorenson_Video_3.wiki` §"Sequence Header" /
§"Slice Header" / §"Packetization" (a verbatim local mirror of the
multimedia.cx Sorenson_Video_3 wiki page, fetched 2026-05-06,
CC-BY-SA per multimedia.cx terms). The SVQ1 `FRAME_SIZE_TABLE` (the
seven standard dimension pairs) is shared via the local `header`
module since both Sorenson formats document the same lookup.

Round 2 was implemented strictly from
`docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
Header" + line 9 (FourCC list) — a verbatim local mirror of the
multimedia.cx Sorenson_Video_1 wiki page (fetched 2026-05-06,
CC-BY-SA per multimedia.cx terms).

No external library source,
no archived `old` branch of this crate, and no online cross-checks
were consulted across any of the four rounds. The wiki spec's
"Appendix A: SVQ1 Data Tables" lists upstream source-tree pointers
for the SVQ1 codebook + VLC byte arrays; those pointers are
explicitly **not** followed. The SVQ3 macroblock-layer sections of
the same wiki page are present in the mirror but were not read in
detail during round 3 (the round scoped to structural sequence /
slice-header parse only); follow-up rounds will tackle them once
the descramble step is documented.
