# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Round 1 — SVQ1 frame-header parser.** `parse_frame_header` walks
  the bit-packed SVQ1 chunk header documented in
  `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
  Header" and returns a typed `Svq1FrameHeader`. Coverage:
  - 22-bit `frame_code` + the `(frame_code & 0x60) == 0`
    invalid-stream check.
  - 8-bit `temporal_reference`.
  - 2-bit `picture_type` (decoded to `Svq1PictureType::Intra` /
    `Predicted` / `Droppable`); value `3` rejected per spec.
  - I-frame trailer chain: optional 16-bit checksum on frame codes
    `0x50` / `0x60`; optional XOR-obfuscated embedded ASCII string
    when `frame_code ^ 0x10 > 0x50` (captured raw, length-prefixed);
    the 2 + 2 + 1 "unknown" sub-fields; 3-bit `frame_size_code`;
    explicit 12 + 12 width/height on code `7`.
  - 1-bit `checksum_present` flag and its `use_packet:1 +
    component:1 + reserved2:2` (reserved2 must be 0) trailer.
  - 1-bit `unknown_flag_1` flag, its 1 + 4 + 1 + 2 fixed-width
    sub-fields, and the `while next-bit-is-1 { read 8 bits }`
    variable-length tail.
  - `FRAME_SIZE_TABLE` constant for the seven standard dimensions.
  - `SVQ1_FOURCC_CODES` constant listing the three FourCC codes the
    spec attaches to SVQ1.
  - MSB-first `BitReader` sized for the header fields.
- Structural error variants: `InvalidFrameCode`,
  `InvalidPictureType`, `InvalidChecksumTrailer`, `Truncated`,
  `BadBitWidth`; the round-0 `NotImplemented` sentinel remains
  available for unwired API surfaces.

### Notes

- **Docs gap (tracked separately):** the SVQ1 multi-stage VQ
  codebooks and per-level VLC tables enumerated in the wiki spec's
  "Appendix A: SVQ1 Data Tables" are not yet pinned in `docs/`.
  Round 2 (the encoded plane-data layer) is blocked on that data
  landing in `docs/video/svq1/spec/` or `docs/video/svq1/tables/`.

### Provenance

Round 1 was implemented strictly from
`docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Stream Format And
Header" — a verbatim local mirror of the multimedia.cx
Sorenson_Video_1 wiki page (fetched 2026-05-06, CC-BY-SA per
multimedia.cx terms). No external library source, no archived `old`
branch of this crate, and no online cross-checks were consulted.

## [0.0.1] — Round 0 — clean-room rebuild scaffold

### Changed

- Clean-room rebuild from a fresh orphan `master`. The previous
  implementation was retired by the OxideAV docs audit dated
  2026-05-06; the prior history is preserved on the `old` branch.
  See `README.md` for the rebuild scope and the strict-isolation
  workspace the Implementer rounds will draw from.
