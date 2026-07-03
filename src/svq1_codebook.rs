//! SVQ1 mean-removed multistage VQ codebook payload (L=0..L=3).
//!
//! ## Provenance
//!
//! The 23004-byte signed-i8 payload + 36-byte descriptor prefix are
//! parsed at build time from `tables/codebook-l0l3.csv` +
//! `tables/codebook-descriptor.csv`, which are bit-exact mirrors of
//! `docs/video/svq1/tables/codebook-{l0l3,descriptor}.csv`. The docs
//! were produced by Extractor 02
//! (`docs/video/svq1/provenance/02-codebook-extraction.md`) from the
//! reference binary `quicktimethirdparty.qtx`
//! SHA-256 `ac3509bf22aa1458dfc6e1af980956c0153b4c287af452ae5b9cac6f923be169`,
//! file offset `0x5d200..0x62c00` (VMA `0x67dcd200..0x67dd2c00`).
//!
//! No external implementation source is consulted.
//!
//! ## Size arithmetic
//!
//! Per `docs/video/svq1/tables/codebook-l0l3.meta`:
//!
//! ```text
//!     L0: vec_len=8B    stage=128B    6-stage codebook=768B
//!     L1: vec_len=16B   stage=256B    6-stage codebook=1536B
//!     L2: vec_len=32B   stage=512B    6-stage codebook=3072B
//!     L3: vec_len=64B   stage=1024B   6-stage codebook=6144B
//! ```
//!
//! The full region is `2 × (768 + 1536 + 3072 + 6144) = 23040 B`,
//! decomposed as a 36-byte descriptor/LUT prefix + 23004-byte vector
//! payload.
//!
//! ## L=4 / L=5 status
//!
//! No codebook exists at L=4 (16×8) or L=5 (16×16) in this build —
//! see [`docs/video/svq1/spec/14.10-codebook-L4.md`] and
//! [`docs/video/svq1/spec/14.11-codebook-L5.md`]. The block-shape LUT
//! at descriptor offset `+0x14` caps quantised block sizes to L=0..L=3
//! (no entry exceeds 4); see
//! [`Svq1Level::codebook_bytes_per_half`] for the codebook size
//! returned per level (`None` for L=4 / L=5).
//!
//! The two `tables/codebook-l{4,5}.meta` records produced by the docs
//! collaborator's Extractor 02 pass (mirrored bit-exact under
//! `crates/oxideav-svq/tables/`) are parsed at build time into the
//! typed [`Svq1AbsentLevelRecord`] constants [`SVQ1_L4_ABSENCE`] and
//! [`SVQ1_L5_ABSENCE`]. The build script verifies that each record's
//! `status` field reads `ABSENT` and that the canonical
//! vector-length / per-half byte-count fields match the values
//! derivable from [`Svq1Level::vector_length`], so a future docs
//! revision that flips a level back to "present" or silently changes
//! the canonical sizes fails the build before any consumer relies on
//! the `None` invariant. See [`Svq1Level::absence_record`] for the
//! ergonomic `Svq1Level → Option<Svq1AbsentLevelRecord>` accessor.
//!
//! ## Within-half vector addressing (pinned)
//!
//! The *within-half* layout — where stage `k`'s vector `v` lives
//! inside one codebook half (intra OR inter) of a single level — IS
//! pinned by `docs/video/svq1/spec/14-codebook-architecture.md` §14.5
//! (addressing convention) and §14.8 (the canonical
//! `half_payload[stage_idx * 16 * V_L + vec_idx * V_L + byte_idx]`
//! arithmetic, stated as the layout that holds "regardless of
//! hypothesis"). [`vector_byte_offset_in_half`] and
//! [`codebook_vector_in_half`] surface that arithmetic: given a
//! caller-supplied half-slice for a level, they resolve a
//! `(stage, vec_idx)` pair to its byte offset / `&[i8]` view. These
//! helpers operate only WITHIN a half the caller already isolated, so
//! they do not depend on the still-open ordering question below.
//!
//! ## Cross-half / cross-level ordering (RESOLVED, Validator role)
//!
//! `docs/video/svq1/spec/14-codebook-architecture.md` §14.8 left the
//! intra-vs-inter ordering and cross-level concatenation as an OPEN
//! item with two working hypotheses (A: intra-first level-ascending;
//! B: level-major intra-then-inter, level-ascending), resolvable by
//! a Validator round. This crate performed that validation against a
//! black-box conformance fixture
//! (`tests/svq1_intra_conformance.rs`): the realised layout is
//! **level-major DESCENDING (L=3 → L=0), intra half then inter half
//! per level**, over the canonical 23 040-byte region whose
//! functional base is `0x5d214` (audit/00 §2.3) — see
//! [`half_byte_offset_in_payload`] for the offsets + evidence and
//! [`vector_byte_to_raster`] for the L=2 / L=3 hierarchical
//! byte→sample tile order the same validation pinned. Both findings
//! are errata for the §14.8 hypothesis pair / §4.7.1 raster claim
//! (flagged for the docs staging).

use crate::svq1_blocktree::Svq1Level;
use crate::svq1_vlc::Svq1Half;

include!(concat!(env!("OUT_DIR"), "/svq1_codebook_data.rs"));

/// Total byte count of the mean-removed VQ vector payload for
/// L=0..L=3 (intra + inter halves), excluding the 36-byte descriptor
/// prefix. See `docs/video/svq1/tables/codebook-l0l3.meta`.
pub const SVQ1_CODEBOOK_PAYLOAD_BYTES: usize = 23004;

/// Total byte count of the 36-byte descriptor + block-shape prefix
/// at file offset `0x5d200..0x5d224` of the reference binary.
pub const SVQ1_CODEBOOK_DESCRIPTOR_BYTES: usize = 36;

/// Length of the block-shape LUT at descriptor offset `+0x14`.
pub const SVQ1_BLOCK_SHAPE_LUT_LEN: usize = 16;

/// Number of mean-removed multistage VQ stages per level (intra or
/// inter half). The SVQ1 wiki references in `docs/video/svq1/wiki/`
/// describe each codebook as a stack of 6 stages of 16 entries; the
/// extracted region size matches that exactly for L=0..L=3.
pub const SVQ1_STAGES_PER_LEVEL: usize = 6;

/// Number of vector entries per stage.
pub const SVQ1_ENTRIES_PER_STAGE: usize = 16;

/// Typed mirror of a `codebook-lN.meta` `status: ABSENT` record under
/// `docs/video/svq1/tables/`.
///
/// Each instance corresponds to one architecturally-absent SVQ1
/// codebook level (L=4 or L=5 in the Sorenson Video TM for QT R2.0
/// build). The fields are exactly the meta file's scalar keys
/// (`level`, `block_size`, `canonical_vector_len_bytes`,
/// `canonical_6stage_intra_or_inter_bytes`) — the multi-line
/// `resolution` and `evidence_rvas` YAML-block scalars are not
/// mirrored here; consult `docs/video/svq1/tables/codebook-l{4,5}.meta`
/// for the binary evidence that pinned each record as ABSENT.
///
/// The `canonical_*` fields document the size the codebook **would**
/// have under the conventional six-level extension of the
/// codebook hierarchy. `canonical_vector_len_bytes` equals
/// `block_cols * block_rows` samples;
/// `canonical_6stage_intra_or_inter_bytes` equals
/// `canonical_vector_len_bytes` multiplied by `16` (entries per
/// stage) and by `6` (stages per level). The values are deliberately
/// kept in the constant so the build assertion can verify that the
/// docs and the code agree on the would-be footprint that was ruled
/// out.
///
/// See [`SVQ1_L4_ABSENCE`] and [`SVQ1_L5_ABSENCE`] for the two
/// instances populated by `build.rs` from
/// `tables/codebook-l{4,5}.meta`, and [`Svq1Level::absence_record`]
/// for the ergonomic accessor that returns one of those constants
/// (or `None` for the L=0..L=3 codebooks that ARE present).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Svq1AbsentLevelRecord {
    /// Level number — `4` or `5`, mirroring the `level:` key.
    pub level: u8,
    /// Block size — `"16x8"` or `"16x16"`, mirroring the
    /// `block_size:` key. `'static str` because the build script
    /// substitutes the literal at compile time.
    pub block_size: &'static str,
    /// Canonical per-vector byte count — `128` (L=4) or `256` (L=5),
    /// mirroring the `canonical_vector_len_bytes:` key. Equal to
    /// `block_cols * block_rows` per the level's spec table.
    pub canonical_vector_len_bytes: u32,
    /// Canonical per-half codebook size in bytes — `12288` (L=4) or
    /// `24576` (L=5), mirroring the
    /// `canonical_6stage_intra_or_inter_bytes:` key. Equal to
    /// `canonical_vector_len_bytes * 16 entries * 6 stages`. The full
    /// would-be codebook (both halves) is twice this value; neither
    /// fits in the reference binary's signed-byte VQ region.
    pub canonical_6stage_intra_or_inter_bytes: u32,
}

impl Svq1Level {
    /// Byte count of the L=`self` codebook for **one** half
    /// (intra OR inter), or `None` for the always-subdivided levels
    /// L=4 and L=5.
    ///
    /// Computed as `vector_length() * SVQ1_ENTRIES_PER_STAGE *
    /// SVQ1_STAGES_PER_LEVEL`. The four returned values
    /// (768, 1536, 3072, 6144) are exactly the per-level breakdown
    /// recorded in `docs/video/svq1/tables/codebook-l0l3.meta`:
    ///
    /// | Level | vec_len | × 16 entries | × 6 stages | bytes/half |
    /// |-------|--------:|-------------:|-----------:|-----------:|
    /// | L=0   |    8    |    128       |    768     |    768     |
    /// | L=1   |   16    |    256       |   1536     |   1536     |
    /// | L=2   |   32    |    512       |   3072     |   3072     |
    /// | L=3   |   64    |   1024       |   6144     |   6144     |
    /// | L=4   |  128    |  (subdivided) |  N/A      |   None     |
    /// | L=5   |  256    |  (subdivided) |  N/A      |   None     |
    ///
    /// Per-half × 2 (intra + inter) summed across L=0..L=3 is
    /// `2 × (768 + 1536 + 3072 + 6144) = 23040 B`, matching the
    /// extracted region total of 23040 B (`= 36 B descriptor +
    /// 23004 B payload`).
    pub const fn codebook_bytes_per_half(self) -> Option<usize> {
        match self {
            Svq1Level::L0 => Some(8 * SVQ1_ENTRIES_PER_STAGE * SVQ1_STAGES_PER_LEVEL),
            Svq1Level::L1 => Some(16 * SVQ1_ENTRIES_PER_STAGE * SVQ1_STAGES_PER_LEVEL),
            Svq1Level::L2 => Some(32 * SVQ1_ENTRIES_PER_STAGE * SVQ1_STAGES_PER_LEVEL),
            Svq1Level::L3 => Some(64 * SVQ1_ENTRIES_PER_STAGE * SVQ1_STAGES_PER_LEVEL),
            Svq1Level::L4 | Svq1Level::L5 => None,
        }
    }

    /// Return the [`Svq1AbsentLevelRecord`] for the always-subdivided
    /// levels (L=4 / L=5), or `None` for the L=0..L=3 levels that DO
    /// have a codebook stored in the reference binary.
    ///
    /// The returned record is one of the [`SVQ1_L4_ABSENCE`] /
    /// [`SVQ1_L5_ABSENCE`] constants that `build.rs` populates from
    /// `tables/codebook-l{4,5}.meta`. Use this when you want to
    /// surface a structured "no codebook at this level" diagnostic
    /// — e.g. when rejecting a malformed bitstream that asks for an
    /// in-place quantisation at L=4 or L=5 (the
    /// [`crate::Error::InvalidLevelQuantise`] case) — without
    /// hard-coding the canonical-size numbers in the caller.
    ///
    /// Invariant guaranteed by the build script:
    /// `self.absence_record().is_some() ==
    ///  self.codebook_bytes_per_half().is_none()` for every
    /// `Svq1Level`. Both predicates fire on exactly the L=4 / L=5 set.
    pub const fn absence_record(self) -> Option<Svq1AbsentLevelRecord> {
        match self {
            Svq1Level::L4 => Some(SVQ1_L4_ABSENCE),
            Svq1Level::L5 => Some(SVQ1_L5_ABSENCE),
            Svq1Level::L0 | Svq1Level::L1 | Svq1Level::L2 | Svq1Level::L3 => None,
        }
    }
}

/// Borrowed view over the full 23004-byte L=0..L=3 codebook payload.
///
/// The interpretation of slices *within* this payload (intra vs inter
/// ordering, stage-vs-level interleave) is still a sibling docs spec
/// task — see the module-level "Open work" note.
pub fn codebook_l0l3_payload() -> &'static [i8] {
    &SVQ1_CODEBOOK_L0L3_BYTES
}

/// Borrowed view over the 36-byte descriptor prefix at file offset
/// `0x5d200..0x5d224` of the reference binary.
pub fn codebook_descriptor() -> &'static [u8] {
    &SVQ1_CODEBOOK_DESCRIPTOR
}

/// Borrowed view over the 16-entry block-shape LUT at descriptor
/// offset `+0x14`. All entries are in the range 1..=4.
pub fn block_shape_lut() -> &'static [u8] {
    &SVQ1_BLOCK_SHAPE_LUT
}

/// Byte offset of mean-removed vector `vec_idx` of stage `stage`
/// within one codebook **half** (intra OR inter) of level `level`.
///
/// Implements the canonical within-half addressing arithmetic pinned
/// by `docs/video/svq1/spec/14-codebook-architecture.md` §14.5
/// (the `(level, half, stage, vec_idx, byte_idx)` convention) and
/// §14.8 (the layout that holds "regardless of hypothesis"):
///
/// ```text
///   offset = stage_idx * 16 * V_L + vec_idx * V_L
/// ```
///
/// where `V_L` is [`Svq1Level::vector_length`], `16` is
/// [`SVQ1_ENTRIES_PER_STAGE`], and `stage_idx = stage - 1` (the spec
/// numbers stages `1..=6` in §14.3; this helper takes that 1-based
/// `stage` directly). The returned offset is the start of the
/// vector's `V_L` bytes within the half; the bytes themselves run in
/// output-raster order per §14.8.
///
/// Returns `None` if:
///
/// * `level` is L=4 or L=5 (no codebook half exists — see
///   [`Svq1Level::codebook_bytes_per_half`]);
/// * `stage` is outside `1..=SVQ1_STAGES_PER_LEVEL` (`1..=6`); or
/// * `vec_idx` is outside `0..SVQ1_ENTRIES_PER_STAGE` (`0..=15`).
///
/// This resolves an offset *within* a half only; it does NOT decide
/// where that half begins inside the contiguous L=0..L=3 payload —
/// that cross-half ordering is the still-open §14.8 item.
///
/// ```
/// use oxideav_svq::svq1_blocktree::Svq1Level;
/// use oxideav_svq::svq1_codebook::vector_byte_offset_in_half;
///
/// // L=0 vectors are 8 bytes: stage 1 vec 0 starts at 0, vec 1 at 8;
/// // stage 2 vec 0 at 16 * 8 = 128.
/// assert_eq!(vector_byte_offset_in_half(Svq1Level::L0, 1, 0), Some(0));
/// assert_eq!(vector_byte_offset_in_half(Svq1Level::L0, 1, 1), Some(8));
/// assert_eq!(vector_byte_offset_in_half(Svq1Level::L0, 2, 0), Some(128));
/// // L=4 has no codebook.
/// assert_eq!(vector_byte_offset_in_half(Svq1Level::L4, 1, 0), None);
/// ```
pub const fn vector_byte_offset_in_half(
    level: Svq1Level,
    stage: usize,
    vec_idx: usize,
) -> Option<usize> {
    let vector_length = level.vector_length() as usize;
    // L=4 / L=5 have no codebook half (vector_length is non-zero for
    // them, so gate on the codebook presence explicitly).
    if level.codebook_bytes_per_half().is_none() {
        return None;
    }
    if stage < 1 || stage > SVQ1_STAGES_PER_LEVEL {
        return None;
    }
    if vec_idx >= SVQ1_ENTRIES_PER_STAGE {
        return None;
    }
    let stage_idx = stage - 1;
    Some(stage_idx * SVQ1_ENTRIES_PER_STAGE * vector_length + vec_idx * vector_length)
}

/// Borrow the `V_L`-byte mean-removed vector for stage `stage`,
/// entry `vec_idx`, of level `level` from a caller-supplied codebook
/// **half** slice.
///
/// `half` is one codebook half (intra OR inter) for the given level —
/// the caller is responsible for isolating it from the contiguous
/// L=0..L=3 payload, because the cross-half / cross-level ordering is
/// the still-open §14.8 item. The returned slice is the vector's
/// `Svq1Level::vector_length` signed bytes in output-raster order, per
/// `docs/video/svq1/spec/14-codebook-architecture.md` §14.8.
///
/// Returns `None` if [`vector_byte_offset_in_half`] returns `None`
/// (absent level / out-of-range `stage` / out-of-range `vec_idx`), or
/// if `half` is too short to contain the addressed vector — i.e.
/// shorter than `offset + V_L`. A correctly-sized half is
/// `Svq1Level::codebook_bytes_per_half(level)` bytes long, which
/// always contains every in-range `(stage, vec_idx)`.
///
/// ```
/// use oxideav_svq::svq1_blocktree::Svq1Level;
/// use oxideav_svq::svq1_codebook::codebook_vector_in_half;
///
/// // A synthetic L=0 half: 768 bytes, each byte equal to its index
/// // modulo 251 cast to i8 (just to make positions distinguishable).
/// let half: Vec<i8> = (0..768).map(|i| (i % 251) as i8).collect();
/// // Stage 1, vector 1 occupies bytes 8..16.
/// let v = codebook_vector_in_half(&half, Svq1Level::L0, 1, 1).unwrap();
/// assert_eq!(v.len(), 8);
/// assert_eq!(v[0], 8);
/// assert_eq!(v[7], 15);
/// ```
pub fn codebook_vector_in_half(
    half: &[i8],
    level: Svq1Level,
    stage: usize,
    vec_idx: usize,
) -> Option<&[i8]> {
    let offset = vector_byte_offset_in_half(level, stage, vec_idx)?;
    let vector_length = level.vector_length() as usize;
    half.get(offset..offset + vector_length)
}

/// Total byte count of the CANONICAL codebook region at file
/// `0x5d214..0x62c14` of the reference binary — exactly the
/// `2 × (768 + 1536 + 3072 + 6144) = 23 040` canonical L=0..L=3
/// intra+inter sum of
/// `docs/video/svq1/spec/14-codebook-architecture.md` §14.4.
///
/// audit/00 §2.3 pinned the codebook's FUNCTIONAL BASE at `0x5d214`
/// (the single reloc-resolved static pointer from the SVC dispatch
/// `.text` range) and §2.5 observed the signed-VQ byte character
/// extends to `0x62c14` (the embedded "BM" bitmap header). The span
/// between those two audited boundaries is exactly canonical —
/// closing audit/00 §7 item 1's "16-byte gap" open item: the first
/// 16 canonical bytes are the block-shape LUT bytes (dual-use), and
/// the last 20 canonical bytes sit past the `codebook-l0l3.csv`
/// extraction window (staged locally as `tables/codebook-tail.csv`).
pub const SVQ1_CODEBOOK_CANONICAL_BYTES: usize = 23040;

/// The full canonical 23 040-byte codebook region (file
/// `0x5d214..0x62c14`): the 16 block-shape-LUT bytes + the
/// 23 004-byte `codebook-l0l3.csv` payload + the 20-byte
/// `codebook-tail.csv` window. See
/// [`SVQ1_CODEBOOK_CANONICAL_BYTES`] for the boundary derivation.
pub fn codebook_canonical() -> &'static [i8] {
    static CELL: std::sync::OnceLock<Vec<i8>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut buf = Vec::with_capacity(SVQ1_CODEBOOK_CANONICAL_BYTES);
        buf.extend(SVQ1_BLOCK_SHAPE_LUT.iter().map(|&b| b as i8));
        buf.extend_from_slice(&SVQ1_CODEBOOK_L0L3_BYTES);
        buf.extend_from_slice(&SVQ1_CODEBOOK_TAIL_BYTES);
        debug_assert_eq!(buf.len(), SVQ1_CODEBOOK_CANONICAL_BYTES);
        buf
    })
}

/// Byte offset of the `(level, half)` codebook page within the
/// canonical 23 040-byte region ([`codebook_canonical`]), resolving
/// the `docs/video/svq1/spec/14-codebook-architecture.md` §14.8
/// open item ("intra-vs-inter ordering within the payload") in the
/// Validator role §14.8 called for:
///
/// ```text
///   intra L=3 at     0   inter L=3 at  6144
///   intra L=2 at 12288   inter L=2 at 15360
///   intra L=1 at 18432   inter L=1 at 19968
///   intra L=0 at 21504   inter L=0 at 22272
/// ```
///
/// i.e. **level-major, levels DESCENDING (L=3 → L=0), intra half
/// then inter half within each level** — neither §14.8 working
/// hypothesis (both assumed level-ascending). Pinned by three
/// independent anchors:
///
/// 1. The black-box intra conformance fixture
///    (`tests/svq1_intra_conformance.rs`): single-stage intra
///    leaves' reference residuals occur VERBATIM in the canonical
///    region exactly at these page offsets (an L=1 leaf's stage-1
///    vec-0 residual at canonical 18432 = the L=1 block start; an
///    L=0 leaf's stage-1 vec-1 residual at 21512 = 21504 + 8), and
///    the full I-frame reconstructs byte-exact.
/// 2. The wiki §"Decoding Intraframe Plane Data" worked-example
///    vectors (stage 1 vec 4 = `7 −16 −10 20 7 −17 −10 20`; stage 2
///    vec 14 = `−13 −6 −1 −4 25 37 −2 −35`) occur in the region
///    EXACTLY ONCE each, 208 bytes apart (the stage-major L=0
///    distance), at offsets placing their page at canonical
///    `22272..23040` — the L=0 SECOND (inter) half, ending exactly
///    at the audited `0x62c14` upper boundary. (The wiki narrates
///    the example on the intraframe path; the bytes live in the
///    level's second half, so the second half being INTER is pinned
///    by anchor 1's intra hits at each level's FIRST half.)
/// 3. The first 16 canonical bytes (the block-shape-LUT dual-use
///    window) read `4 4 3 2 4 3 3 2 3 3 2 2 3 2 2 1` — a smooth
///    decaying 4×4 patch, structurally consistent with an intra
///    L=3 stage-1 vec-0 leading quadrant.
///
/// The within-half layout stays the canonical §14.5 stage-major
/// arithmetic (`stage_idx × 16 × V_L + vec_idx × V_L`), confirmed
/// by anchor 2 (208-byte distance); the BYTE → SAMPLE order within
/// one vector is hierarchical for L=2 / L=3 — see
/// [`vector_byte_to_raster`]. Returns `None` for the absent L=4 /
/// L=5 levels.
pub const fn half_byte_offset_in_payload(level: Svq1Level, half: Svq1Half) -> Option<usize> {
    // Level-major DESCENDING: [L=3 block 12288 B][L=2 block 6144 B]
    // [L=1 block 3072 B][L=0 block 1536 B]; each level block is
    // [intra half][inter half].
    let (intra_off, inter_off) = match level {
        Svq1Level::L3 => (0usize, 6144),
        Svq1Level::L2 => (12288, 15360),
        Svq1Level::L1 => (18432, 19968),
        Svq1Level::L0 => (21504, 22272),
        Svq1Level::L4 | Svq1Level::L5 => return None,
    };
    match half {
        Svq1Half::Intra => Some(intra_off),
        Svq1Half::Inter => Some(inter_off),
    }
}

/// Map a codebook-vector BYTE index to its output-raster sample
/// position within the block, for level `level`.
///
/// The vector bytes are NOT stored in whole-block raster order for
/// the two multi-4×4 levels; they follow the block-subdivision
/// hierarchy (the same top/bottom-then-left/right split order as
/// `docs/video/svq1/spec/03-block-hierarchy.md` §3.4), bottoming
/// out in 4×4 raster tiles:
///
/// * **L=3 (8×8):** four 16-byte 4×4 tiles in the order top-left,
///   top-right, bottom-left, bottom-right.
/// * **L=2 (8×4):** two 16-byte 4×4 tiles in the order left, right.
/// * **L=1 (4×4) / L=0 (4×2):** plain raster (the hierarchical
///   order and the raster order coincide at or below one tile).
///
/// Pinned empirically by the black-box intra conformance fixture:
/// single-stage L=3 leaves' reference residuals equal the addressed
/// codebook vector under exactly this byte→sample order (and match
/// NO whole-block-raster window anywhere in the canonical region).
/// This corrects the whole-block-raster reading of spec/04 §4.7.1 —
/// an erratum for the docs staging.
pub const fn vector_byte_to_raster(level: Svq1Level, byte_idx: usize) -> usize {
    let (block_w, _) = level.block_dims();
    let block_w = block_w as usize;
    if block_w <= 4 {
        // L=0 / L=1: one tile — raster order directly.
        return byte_idx;
    }
    // L=2 / L=3: 16-byte 4×4 tiles. Tile order: L=2 → left, right;
    // L=3 → top-left, top-right, bottom-left, bottom-right.
    let tile = byte_idx / 16;
    let within = byte_idx % 16;
    let (tile_x, tile_y) = (tile % 2, tile / 2);
    let (wx, wy) = (within % 4, within / 4);
    (tile_y * 4 + wy) * block_w + tile_x * 4 + wx
}

/// Borrow the `(level, half)` codebook page from the canonical
/// codebook region (see [`half_byte_offset_in_payload`] for the
/// empirically-pinned page layout). Every page is exactly
/// `Svq1Level::codebook_bytes_per_half` bytes. Returns `None` for
/// L=4 / L=5.
pub fn codebook_half(level: Svq1Level, half: Svq1Half) -> Option<&'static [i8]> {
    let start = half_byte_offset_in_payload(level, half)?;
    let len = level.codebook_bytes_per_half()?;
    codebook_canonical().get(start..start + len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_length_matches_documented_size() {
        // docs/video/svq1/tables/codebook-l0l3.meta: byte_length: 23004
        assert_eq!(SVQ1_CODEBOOK_L0L3_BYTES.len(), SVQ1_CODEBOOK_PAYLOAD_BYTES);
        assert_eq!(SVQ1_CODEBOOK_PAYLOAD_BYTES, 23004);
    }

    #[test]
    fn descriptor_length_matches_documented_size() {
        // docs/video/svq1/tables/codebook-descriptor.meta: byte_length: 36
        assert_eq!(
            SVQ1_CODEBOOK_DESCRIPTOR.len(),
            SVQ1_CODEBOOK_DESCRIPTOR_BYTES
        );
        assert_eq!(SVQ1_CODEBOOK_DESCRIPTOR_BYTES, 36);
    }

    #[test]
    fn full_region_matches_size_arithmetic() {
        // codebook-l0l3.meta line 24: "region total 23040 B = 23040 B
        // canonical (= 36 B over the 23004 B vector payload; the 36 B
        // is the descriptor/LUT prefix)."
        let descriptor_plus_payload =
            SVQ1_CODEBOOK_DESCRIPTOR.len() + SVQ1_CODEBOOK_L0L3_BYTES.len();
        assert_eq!(descriptor_plus_payload, 23040);

        // Per-half canonical L0..L3 sum:
        // 768 + 1536 + 3072 + 6144 = 11520; both halves = 23040.
        let per_half = Svq1Level::L0.codebook_bytes_per_half().unwrap()
            + Svq1Level::L1.codebook_bytes_per_half().unwrap()
            + Svq1Level::L2.codebook_bytes_per_half().unwrap()
            + Svq1Level::L3.codebook_bytes_per_half().unwrap();
        assert_eq!(per_half, 11520);
        assert_eq!(per_half * 2, 23040);
        assert_eq!(per_half * 2, descriptor_plus_payload);
    }

    #[test]
    fn per_level_byte_counts_match_docs_meta() {
        // docs/video/svq1/tables/codebook-l0l3.meta lines 26-29
        assert_eq!(Svq1Level::L0.codebook_bytes_per_half(), Some(768));
        assert_eq!(Svq1Level::L1.codebook_bytes_per_half(), Some(1536));
        assert_eq!(Svq1Level::L2.codebook_bytes_per_half(), Some(3072));
        assert_eq!(Svq1Level::L3.codebook_bytes_per_half(), Some(6144));
    }

    #[test]
    fn l4_l5_have_no_codebook() {
        // docs/video/svq1/spec/14.10-codebook-L4.md + 14.11-codebook-L5.md
        // RESOLVED ABSENT — block-shape LUT caps at 4 (four block
        // sizes); 16×8 and 16×16 are always subdivided.
        assert_eq!(Svq1Level::L4.codebook_bytes_per_half(), None);
        assert_eq!(Svq1Level::L5.codebook_bytes_per_half(), None);
    }

    #[test]
    fn block_shape_lut_matches_descriptor_csv() {
        // docs/video/svq1/tables/codebook-descriptor.meta line 22:
        // "Block-shape LUT bytes: 04 04 03 02 04 03 03 02 03 03 02 02 03 02 02 01."
        let expected: [u8; 16] = [
            0x04, 0x04, 0x03, 0x02, 0x04, 0x03, 0x03, 0x02, 0x03, 0x03, 0x02, 0x02, 0x03, 0x02,
            0x02, 0x01,
        ];
        assert_eq!(SVQ1_BLOCK_SHAPE_LUT, expected);
        assert_eq!(block_shape_lut(), &expected[..]);
    }

    #[test]
    fn block_shape_lut_caps_at_four() {
        // Every entry must be in 1..=4 per the spec/14.10 + 14.11
        // resolution: a value of 5 or 6 would imply an L=4 or L=5
        // codebook is consulted in place; none exists.
        for &v in block_shape_lut() {
            assert!(
                (1..=4).contains(&v),
                "block-shape LUT entry {v} out of 1..=4"
            );
        }
    }

    #[test]
    fn descriptor_csv_first_record_matches() {
        // docs/video/svq1/tables/codebook-descriptor.csv row 1 (after
        // header): byte 0 = 0x03, byte 3 = 0x18, byte 4 = 0x02.
        // The descriptor's first 9-byte record b0 cycles 5..0 per the
        // meta line 20-21 "Two identical descriptor groups precede the
        // LUT (0x5d1cc and 0x5d1f4); each enumerates the full
        // six-level block hierarchy 5..0".
        assert_eq!(SVQ1_CODEBOOK_DESCRIPTOR[0], 0x03);
        assert_eq!(SVQ1_CODEBOOK_DESCRIPTOR[3], 0x18);
        assert_eq!(SVQ1_CODEBOOK_DESCRIPTOR[4], 0x02);
        // Block-shape LUT starts at offset 0x14
        assert_eq!(SVQ1_CODEBOOK_DESCRIPTOR[0x14], 0x04);
    }

    #[test]
    fn codebook_l0l3_first_bytes_match_hex_table_row1() {
        // docs/video/svq1/tables/codebook-l0l3.hex row 1:
        // "02 01 00 ff 01 00 ff ff 01 00 ff fe 00 ff fe fd"
        // The CSV/hex pair are extracted from the same source slice,
        // so checking the first 16 bytes anchors the build-script
        // CSV parse to the documented hex view.
        let expected_signed: [i8; 16] = [
            0x02, 0x01, 0x00, -1, 0x01, 0x00, -1, -1, 0x01, 0x00, -1, -2, 0x00, -1, -2, -3,
        ];
        assert_eq!(&SVQ1_CODEBOOK_L0L3_BYTES[..16], &expected_signed[..]);
    }

    #[test]
    fn codebook_l0l3_payload_accessor_aliases_static() {
        let p = codebook_l0l3_payload();
        assert_eq!(p.len(), 23004);
        assert_eq!(p.as_ptr(), SVQ1_CODEBOOK_L0L3_BYTES.as_ptr());
    }

    #[test]
    fn codebook_descriptor_accessor_aliases_static() {
        let d = codebook_descriptor();
        assert_eq!(d.len(), 36);
        assert_eq!(d.as_ptr(), SVQ1_CODEBOOK_DESCRIPTOR.as_ptr());
    }

    #[test]
    fn svq1_l4_absence_matches_meta_constants() {
        // tables/codebook-l4.meta: level=4, block_size=16x8,
        // canonical_vector_len_bytes=128,
        // canonical_6stage_intra_or_inter_bytes=12288, status=ABSENT.
        // The build script asserts `status == ABSENT` at compile
        // time — here we just sanity-check the scalar fields the
        // generated constant carries forward.
        assert_eq!(SVQ1_L4_ABSENCE.level, 4);
        assert_eq!(SVQ1_L4_ABSENCE.block_size, "16x8");
        assert_eq!(SVQ1_L4_ABSENCE.canonical_vector_len_bytes, 128);
        assert_eq!(SVQ1_L4_ABSENCE.canonical_6stage_intra_or_inter_bytes, 12288);
        // Per-half byte count must equal vector_len * 16 entries * 6 stages.
        assert_eq!(
            SVQ1_L4_ABSENCE.canonical_6stage_intra_or_inter_bytes,
            SVQ1_L4_ABSENCE.canonical_vector_len_bytes
                * SVQ1_ENTRIES_PER_STAGE as u32
                * SVQ1_STAGES_PER_LEVEL as u32
        );
    }

    #[test]
    fn svq1_l5_absence_matches_meta_constants() {
        // tables/codebook-l5.meta: level=5, block_size=16x16,
        // canonical_vector_len_bytes=256,
        // canonical_6stage_intra_or_inter_bytes=24576, status=ABSENT.
        assert_eq!(SVQ1_L5_ABSENCE.level, 5);
        assert_eq!(SVQ1_L5_ABSENCE.block_size, "16x16");
        assert_eq!(SVQ1_L5_ABSENCE.canonical_vector_len_bytes, 256);
        assert_eq!(SVQ1_L5_ABSENCE.canonical_6stage_intra_or_inter_bytes, 24576);
        assert_eq!(
            SVQ1_L5_ABSENCE.canonical_6stage_intra_or_inter_bytes,
            SVQ1_L5_ABSENCE.canonical_vector_len_bytes
                * SVQ1_ENTRIES_PER_STAGE as u32
                * SVQ1_STAGES_PER_LEVEL as u32
        );
    }

    #[test]
    fn absence_record_canonical_sizes_match_vector_length() {
        // The build-asserted `canonical_vector_len_bytes` field of
        // each absence record must agree with the corresponding
        // `Svq1Level::vector_length()` value. This is the property
        // that lets `Svq1Level::absence_record` stand in for the
        // raw meta-file content.
        assert_eq!(
            SVQ1_L4_ABSENCE.canonical_vector_len_bytes,
            Svq1Level::L4.vector_length() as u32
        );
        assert_eq!(
            SVQ1_L5_ABSENCE.canonical_vector_len_bytes,
            Svq1Level::L5.vector_length() as u32
        );
    }

    #[test]
    fn absence_record_accessor_returns_l4_l5_records() {
        // L=4 / L=5 → Some(record); L=0..L=3 → None.
        assert_eq!(Svq1Level::L4.absence_record(), Some(SVQ1_L4_ABSENCE));
        assert_eq!(Svq1Level::L5.absence_record(), Some(SVQ1_L5_ABSENCE));
        assert_eq!(Svq1Level::L0.absence_record(), None);
        assert_eq!(Svq1Level::L1.absence_record(), None);
        assert_eq!(Svq1Level::L2.absence_record(), None);
        assert_eq!(Svq1Level::L3.absence_record(), None);
    }

    #[test]
    fn absence_and_codebook_predicates_are_complementary() {
        // Documented invariant on Svq1Level::absence_record: an L=N
        // for which `absence_record()` returns Some MUST be the same
        // L=N for which `codebook_bytes_per_half()` returns None,
        // and vice versa. This locks the two surfaces together.
        for level in [
            Svq1Level::L0,
            Svq1Level::L1,
            Svq1Level::L2,
            Svq1Level::L3,
            Svq1Level::L4,
            Svq1Level::L5,
        ] {
            assert_eq!(
                level.absence_record().is_some(),
                level.codebook_bytes_per_half().is_none(),
                "absence_record / codebook_bytes_per_half disagree at {level:?}"
            );
        }
    }

    /// The four present levels, paired with their vector length.
    const PRESENT_LEVELS: [(Svq1Level, usize); 4] = [
        (Svq1Level::L0, 8),
        (Svq1Level::L1, 16),
        (Svq1Level::L2, 32),
        (Svq1Level::L3, 64),
    ];

    #[test]
    fn vector_offset_first_vector_is_zero_for_every_present_level() {
        // §14.8: half_payload[stage_idx * 16 * V_L + vec_idx * V_L].
        // Stage 1 (stage_idx 0), vec 0 → offset 0 for every level.
        for (level, _) in PRESENT_LEVELS {
            assert_eq!(vector_byte_offset_in_half(level, 1, 0), Some(0));
        }
    }

    #[test]
    fn vector_offset_matches_canonical_arithmetic() {
        // Spot-check the closed form against an independent recompute
        // across the full (stage, vec_idx) grid for each present level.
        for (level, v_l) in PRESENT_LEVELS {
            for stage in 1..=SVQ1_STAGES_PER_LEVEL {
                for vec_idx in 0..SVQ1_ENTRIES_PER_STAGE {
                    let expected = (stage - 1) * SVQ1_ENTRIES_PER_STAGE * v_l + vec_idx * v_l;
                    assert_eq!(
                        vector_byte_offset_in_half(level, stage, vec_idx),
                        Some(expected),
                        "offset mismatch at {level:?} stage {stage} vec {vec_idx}"
                    );
                }
            }
        }
    }

    #[test]
    fn vector_offset_last_entry_ends_exactly_at_half_size() {
        // The final addressable byte (stage 6, vec 15, last byte of the
        // vector) must be the last byte of the half: offset + V_L ==
        // codebook_bytes_per_half(level).
        for (level, v_l) in PRESENT_LEVELS {
            let last = vector_byte_offset_in_half(
                level,
                SVQ1_STAGES_PER_LEVEL,
                SVQ1_ENTRIES_PER_STAGE - 1,
            )
            .unwrap();
            assert_eq!(
                last + v_l,
                level.codebook_bytes_per_half().unwrap(),
                "last vector of {level:?} does not end at the half boundary"
            );
        }
    }

    #[test]
    fn vector_offset_rejects_absent_levels() {
        assert_eq!(vector_byte_offset_in_half(Svq1Level::L4, 1, 0), None);
        assert_eq!(vector_byte_offset_in_half(Svq1Level::L5, 1, 0), None);
    }

    #[test]
    fn vector_offset_rejects_out_of_range_stage() {
        // Stage is 1-based 1..=6; 0 and 7 are out of range.
        assert_eq!(vector_byte_offset_in_half(Svq1Level::L0, 0, 0), None);
        assert_eq!(
            vector_byte_offset_in_half(Svq1Level::L0, SVQ1_STAGES_PER_LEVEL + 1, 0),
            None
        );
        // The boundary stage 6 IS valid.
        assert!(vector_byte_offset_in_half(Svq1Level::L0, SVQ1_STAGES_PER_LEVEL, 0).is_some());
    }

    #[test]
    fn vector_offset_rejects_out_of_range_vec_idx() {
        assert_eq!(
            vector_byte_offset_in_half(Svq1Level::L0, 1, SVQ1_ENTRIES_PER_STAGE),
            None
        );
        // The boundary entry 15 IS valid.
        assert!(vector_byte_offset_in_half(Svq1Level::L0, 1, SVQ1_ENTRIES_PER_STAGE - 1).is_some());
    }

    #[test]
    fn vector_offsets_are_unique_and_cover_the_half() {
        // Every (stage, vec_idx) maps to a distinct, V_L-aligned
        // offset, and the set of offsets exactly tiles the half with no
        // gaps or overlaps.
        for (level, v_l) in PRESENT_LEVELS {
            let mut seen = vec![false; level.codebook_bytes_per_half().unwrap() / v_l];
            for stage in 1..=SVQ1_STAGES_PER_LEVEL {
                for vec_idx in 0..SVQ1_ENTRIES_PER_STAGE {
                    let off = vector_byte_offset_in_half(level, stage, vec_idx).unwrap();
                    assert_eq!(off % v_l, 0, "offset {off} not V_L-aligned at {level:?}");
                    let slot = off / v_l;
                    assert!(!seen[slot], "duplicate slot {slot} at {level:?}");
                    seen[slot] = true;
                }
            }
            assert!(
                seen.iter().all(|&b| b),
                "offsets do not tile the full half for {level:?}"
            );
        }
    }

    #[test]
    fn vector_borrow_returns_correct_length_and_bytes() {
        // Synthetic half whose byte j holds (j % 251) as i8 so each
        // position is identifiable.
        for (level, v_l) in PRESENT_LEVELS {
            let half_len = level.codebook_bytes_per_half().unwrap();
            let half: Vec<i8> = (0..half_len).map(|j| (j % 251) as i8).collect();
            for stage in 1..=SVQ1_STAGES_PER_LEVEL {
                for vec_idx in 0..SVQ1_ENTRIES_PER_STAGE {
                    let off = vector_byte_offset_in_half(level, stage, vec_idx).unwrap();
                    let v = codebook_vector_in_half(&half, level, stage, vec_idx).unwrap();
                    assert_eq!(v.len(), v_l, "wrong vector length at {level:?}");
                    for (k, &byte) in v.iter().enumerate() {
                        assert_eq!(byte, ((off + k) % 251) as i8);
                    }
                }
            }
        }
    }

    #[test]
    fn vector_borrow_rejects_short_half() {
        // A half one byte too short to hold the last vector returns None
        // rather than panicking.
        let level = Svq1Level::L0;
        let v_l = 8;
        let full = level.codebook_bytes_per_half().unwrap();
        let short: Vec<i8> = vec![0; full - 1];
        // Last vector needs bytes [full - v_l .. full); the half ends at
        // full - 1, so the borrow fails.
        assert_eq!(
            codebook_vector_in_half(
                &short,
                level,
                SVQ1_STAGES_PER_LEVEL,
                SVQ1_ENTRIES_PER_STAGE - 1
            ),
            None
        );
        // But a half exactly v_l shorter still serves all but the last.
        assert!(
            codebook_vector_in_half(&short, level, SVQ1_STAGES_PER_LEVEL, 0).is_some(),
            "earlier vectors should still be addressable in a one-byte-short half"
        );
        let _ = v_l;
    }

    #[test]
    fn vector_borrow_rejects_absent_and_out_of_range() {
        let dummy: Vec<i8> = vec![0; 64];
        assert_eq!(codebook_vector_in_half(&dummy, Svq1Level::L4, 1, 0), None);
        assert_eq!(codebook_vector_in_half(&dummy, Svq1Level::L0, 0, 0), None);
        assert_eq!(
            codebook_vector_in_half(&dummy, Svq1Level::L0, 1, SVQ1_ENTRIES_PER_STAGE),
            None
        );
    }

    #[test]
    fn vector_offset_is_const_usable() {
        const OFF: Option<usize> = vector_byte_offset_in_half(Svq1Level::L3, 2, 3);
        // L=3 V_L = 64: stage 2 (idx 1) vec 3 → 1*16*64 + 3*64 = 1216.
        assert_eq!(OFF, Some(16 * 64 + 3 * 64));
        assert_eq!(OFF, Some(1216));
    }
}
