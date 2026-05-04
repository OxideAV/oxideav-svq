# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added (round 3 — diagnostic)

- `tests/trace_first_mbs.rs` — bit-by-bit walker trace of the first 20
  MBs of an ffmpeg-encoded testsrc 176×144 fixture. Used to verify
  the bit-reader and walker discipline. Skipped if ffmpeg/ffprobe
  aren't on PATH.

### Investigated (round 3)

- **The bit-reader and the multistage VLC walker are bit-position
  correct.** Every split-flag, count-VLC, mean-VLC and 4-bit codebook
  index our decoder reads on the testsrc fixture matches the actual
  bitstream bytes (verified by hand against §14 of the trace doc,
  through the entire first two MBs of a 176×144 testsrc I-frame).
- **The trace doc's empirical claim that `count == 0` is the only
  legal multistage outcome at L=4/L=5 is contradicted by the FFmpeg
  encoder we validate against.** MB(32,0) of testsrc-176×144 is
  encoded as a single L=5 leaf with `count=2`, `mean=26`, two
  4-bit codebook indices `[10, 12]` — observed at body bit position
  305 in our trace (cross-checked against the raw packet bytes:
  `8f 56` at packet bytes 38–39 unpacks to exactly that pattern).
  Per §7.2 of the trace doc, decoding such a leaf requires the
  L=5 (16×16) intra codebook (6 stages × 16 entries × 256 bytes =
  24 576 bytes) — which §14.7 / §14.8 do **not** transcribe.
- Round 2's BLOCKED memo was correct in spirit: the L=4 (16×8) and
  L=5 (16×16) codebooks **are** required for real testsrc decode.
  Round 2's "consume the codebook indices but skip the additive
  contribution" workaround keeps the bit-reader aligned but leaves
  every L=4/5 leaf at mean-only fill, which is the source of the
  ~10–11 dB Y PSNR floor on the testsrc roundtrip test.

### Blocked (round 3 — docs collaborator)

- **L=4 (16×8) and L=5 (16×16) intra/inter codebook bytes.** Need to
  be transcribed into the trace doc as §14.10 / §14.11 (or as an
  appendix to §14.7) to unblock real testsrc decoding. Per §7.2 the
  shape is 6 stages × 16 entries × `pixels_at_level(L)` signed
  bytes, i.e. 12 288 bytes for L=4 and 24 576 bytes for L=5 per
  intra/inter set. The §7 / §14.7 prose claiming these slots are
  "intentionally null" must also be corrected — empirical FFmpeg
  encoder output proves they're populated.
- Once the codebooks land, replace the "consume-and-skip" path in
  `src/v1/vq.rs::decode_intra_leaf` with `intra_codebook_for_level`
  returning `Some(_)` for `L=4..=5`, and tighten the L=4/5 `count > 0`
  path to a real `add_into` call.

### Added (round 2 — partial)

- Multistage VLC tables: block-type, intra & inter multistage (6 levels
  × 8 codes each), 256-entry intra mean, 512-entry inter mean,
  33-entry motion-component magnitude. Transcribed verbatim from §14
  of the trace doc (`docs/video/svq1/svq1-trace-reverse-engineering.md`).
- Generic `Vlc` decoder (`src/v1/vlc.rs`) with sequential
  prefix-match — same pattern as oxideav-jpeg2000 / oxideav-jpegxl.
  Every table round-trips encode → decode in unit tests.
- Four fixed VQ codebooks (4×2, 4×4, 8×4, 8×8) for both intra and
  inter sets — 22 272 signed bytes per set, 44 544 total. Verbatim
  from §14.8–§14.9.
- Hierarchical quad-tree walker with depth-first recursion, split-axis
  alternation (height-halved at odd levels, width-halved at even),
  and per-leaf reconstruction (`mean + Σ codebook_stage[idx]`, clipped
  to `[0,255]`). Handles I-frames end-to-end at the bit-position level.
- Solid-gray testsrc fixture decodes **bit-exact** (PSNR = +∞ dB) —
  exercises the multistage VLC + mean VLC at L=5 leaves.

### Blocked (round 2)

- **L=4 (16×8) and L=5 (16×16) codebooks** are not in the trace doc
  (§14.7 lists only the four 4×2 / 4×4 / 8×4 / 8×8 codebooks, total
  22 272 bytes per intra/inter set, not the 44 544 the trace-doc
  prose claims overall). The trace doc states `count > 0` at L=4/5
  is a bitstream error, but FFmpeg-encoded testsrc fixtures routinely
  produce `count > 0` there (we observed `count = 6` at L=5 in a
  16×16 testsrc fixture). Without the L=4/5 codebooks the walker
  consumes the codebook-index bits to keep the bit-reader aligned but
  skips the additive contribution — bit-position-correct, pixel-
  partially-correct. PSNR on testsrc 176×144 is ~9-11 dB pending the
  missing codebook tables. See the BLOCKED memo at the foot of
  `src/v1/codebook.rs` for the exact derivation paths attempted.
- P-frame motion compensation + INTER VQ residual — round 3.
- Header-byte-swap obfuscation pre-pass (`frame_code != 0x20`) —
  round 3.
- Header `frame_code in {0x50, 0x60}` checksum + scrambled-string
  paths — round 3.

## [0.0.1] - 2026-05-02

### Added

- Initial scaffold of the pure-Rust Sorenson SVQ1 (FourCC `SVQ1`)
  video decoder. Parses the 22-bit `frame_code` packet prefix, the
  I-/P-frame header (temporal reference, frame type, preset/explicit
  frame size, optional checksum and extra-data blocks), and decodes
  I-frame bodies via hierarchical multistage vector quantisation
  using fixed codebooks. Output is YUV 4:1:0 in an
  `oxideav_core::VideoFrame`. P-frames currently surface as
  `Error::Unsupported` (no motion-compensation pipeline yet); see
  the README "Gaps" section for the full list.
