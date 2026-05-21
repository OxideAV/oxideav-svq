# oxideav-svq

Pure-Rust Sorenson Video (SVQ1 / SVQ3) codec for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Round 1 — structural SVQ1 frame-header parse.** The orphan rebuild
of this `master` (round 0) lands its first decode milestone:
[`parse_frame_header`](src/header.rs) walks every bit of the SVQ1
chunk header documented in
[`docs/video/svq1/wiki/Sorenson_Video_1.wiki`](https://github.com/OxideAV/docs/blob/master/video/svq1/wiki/Sorenson_Video_1.wiki)
§"Stream Format And Header" and returns a typed
`Svq1FrameHeader`.

What round 1 covers:

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

What round 1 does **not** yet cover:

* The encoded plane data (`Y` / `U` / `V`) that follows the header.
  Decoding those requires the SVQ1 multi-stage VQ codebooks and the
  per-level VLC tables enumerated in the spec's "Appendix A: SVQ1
  Data Tables" — both blocked on the open docs-collaborator task
  tracking the codebook byte-list extraction.
* Embedded-string de-obfuscation. The parser captures the raw
  obfuscated bytes plus the declared length; de-obfuscation is
  deferred until the `string_xor_table[]` is pinned in `docs/`.
* Checksum verification. The spec itself notes "The specific details
  of the checksum coding are not all known"; the value is captured
  for future verification once the algorithm is documented.
* SVQ3. The `docs/video/svq3/wiki/Sorenson_Video_3.wiki` snapshot
  exists but a round-1 prompt scoped to SVQ1.
* `oxideav-core` framework integration. The crate is currently a
  self-contained structural parser; a subsequent round will add a
  default-on `registry` cargo feature that wires the parser into
  `CodecResolver` via the FourCC declarations.

## Clean-room provenance

Round 1 was implemented strictly from
`docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
Header" — a verbatim local mirror of the multimedia.cx
Sorenson_Video_1 wiki page (fetched 2026-05-06, CC-BY-SA per
multimedia.cx terms). No external library source, no archived `old`
branch of this crate, and no online cross-checks were consulted.
Codebook / VLC-table contents were not read or transcribed; they
will land behind the documented docs-gap once that gap is closed.

The wiki spec's "Appendix A: SVQ1 Data Tables" lists upstream
source-tree pointers for the codebook + VLC byte arrays. Those
pointers are explicitly **not** followed; round 1 implements the
structural-header layer alone and the docs-gap note above tracks
the upstream blocker for the data-table layer.
