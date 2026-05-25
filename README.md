# oxideav-svq

Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Round 5 — SVQ3 residual coefficient walker.** The per-block
Golomb-coded `(run, value)` residual coefficient stream from the
wiki spec's §"Coefficient decoding" now lands in the new
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

## Clean-room provenance

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

No external library source (FFmpeg / libavcodec / MPlayer / etc.),
no archived `old` branch of this crate, and no online cross-checks
were consulted across any of the four rounds. The wiki spec's
"Appendix A: SVQ1 Data Tables" lists upstream source-tree pointers
for the SVQ1 codebook + VLC byte arrays; those pointers are
explicitly **not** followed. The SVQ3 macroblock-layer sections of
the same wiki page are present in the mirror but were not read in
detail during round 3 (the round scoped to structural sequence /
slice-header parse only); follow-up rounds will tackle them once
the descramble step is documented.
