# oxideav-svq

Pure-Rust **Sorenson Video 1 (SVQ1)** video decoder for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.
SVQ1 (FourCC `SVQ1`) is the late-1990s Apple QuickTime hierarchical-
multistage VQ codec used between roughly 1998 and 2002.  Zero C
dependencies.

## Status

| Feature                                                    | Status   |
|------------------------------------------------------------|----------|
| 22-bit `frame_code` parse + legality test                  | works    |
| `temporal_reference` + `frame_type`                        | works    |
| I-frame `reserved` + `frame_size_code` + preset/explicit   | works    |
| `checksum_block_flag` skip path                            | works    |
| `extra_data_block_flag` + stop-1+u(8) chain skip           | works    |
| YUV 4:1:0 → YUV 4:2:0 chroma upsample                      | works    |
| `Decoder` trait wiring + `Codec` registration              | works    |
| Multistage VLC walker + L=0..3 (4×2/4×4/8×4/8×8) codebooks | works (~10 dB Y PSNR on testsrc, blocked on missing L=4/5 codebooks for ceiling) |
| Solid-gray fixture (every leaf is L=5 mean-only)           | bit-exact |
| L=4 (16×8) + L=5 (16×16) codebooks                         | **blocked on docs gap** (see below) |
| P-frame motion-compensation                                | gap      |
| Header obfuscation pre-pass for `frame_code != 0x20`       | gap      |
| Optional packet checksum / embedded-string scrambling      | skipped (consumed but not validated/decoded) |

## Round-2/3 partial-decode body pipeline

The SVQ1 body is a hierarchical multistage VQ with **fixed codebooks**:
six levels × six stages × sixteen entries × per-level pixel count of
signed 8-bit perturbations, plus six per-level multistage VLC tables
(intra + inter), an intra mean VLC, an inter mean VLC and an H.263-
style motion-component VLC.

Round-2 landed the multistage walker, the four 4×2/4×4/8×4/8×8
codebooks, the multistage and mean VLC decoders, and end-to-end
ffmpeg-roundtrip integration tests. Solid-gray fixtures decode
bit-exact. Real-content fixtures decode to a roughly ~10 dB Y PSNR
floor — explained below.

Round-3 verified bit-by-bit through the first two MBs of a
ffmpeg-encoded testsrc 176×144 I-frame that the bit-reader and the
multistage walker are bit-position correct. The PSNR floor is **not**
a bit-reader bug. It is a missing-data gap in the trace doc: the
FFmpeg encoder routinely emits `count > 0` at L=4 and L=5 leaves
(e.g. MB(32,0) of testsrc-176×144 is a single L=5 leaf with
`count=2 mean=26 indices=[10,12]`), but `docs/video/svq1/svq1-
trace-reverse-engineering.md` §14.7 does NOT transcribe the
L=4 (16×8) or L=5 (16×16) codebooks (and §7 incorrectly claims
they don't exist). Until those bytes land in the trace doc, the
walker keeps the bit-reader aligned at L=4/5 leaves with `count > 0`
by consuming the index bits but skipping the additive contribution
— so the result is mean-only fill at those leaves, hence the ~10 dB
floor.

## Gaps

- **L=4 (16×8) + L=5 (16×16) intra/inter VQ codebooks.** Round-3
  blocker. Need to be transcribed as §14.10 / §14.11 of the trace
  doc. Shape per §7.2 / §14.7 prose: 6 stages × 16 entries ×
  `pixels_at_level(L)` signed bytes (12 288 bytes for L=4, 24 576
  bytes for L=5, per intra/inter set). Once landed, the
  `consume-and-skip` branch in `src/v1/vq.rs::decode_intra_leaf` is
  replaced by a real `add_into` call against
  `intra_codebook_for_level(4|5)`, and the testsrc PSNR rises from
  ~10 dB toward the codec's natural ceiling. See the round-3
  CHANGELOG entry and `tests/trace_first_mbs.rs` for the empirical
  bit-by-bit demonstration.
- **P-frame motion-compensation.** Block-type VLC, MV component VLC +
  median predictor, half-pel bilinear interpolation, 4V sub-block
  variant — unimplemented. Today the decoder produces mid-grey for P-
  frames too, so the temporal sequence still flows but no motion is
  reconstructed.
- **Header obfuscation pre-pass.** When `frame_code in {0x40, 0x50,
  0x60}` the decoder must un-swap-and-XOR bytes `[4..36)` of the
  packet before parsing the rest of the header. Our trace corpus is
  100 % `frame_code = 0x20` so we surface a clear `unsupported`
  error for the obfuscated path rather than half-implementing it.
- **`frame_size_code = 4`.** Not exercised by any sample in the trace
  corpus; the table entry is left blank and the decoder rejects the
  code as `unsupported` (rather than guessing).

## Installation

```toml
[dependencies]
oxideav-core = "0.1"
oxideav-svq = "0.0"
```

## Quick use

```rust,no_run
use oxideav_core::CodecRegistry;

let mut codecs = CodecRegistry::new();
oxideav_svq::register(&mut codecs);
```

The decoder claims the QuickTime FourCC `SVQ1` (and its lowercase
`svq1` spelling sometimes seen in MOV `stsd` boxes); the container
crates (oxideav-mp4) recognise both.

## References

- `docs/video/svq1/svq1-trace-reverse-engineering.md` — primary
  clean-room behavioural-trace spec.
- MultimediaWiki — *Sorenson Video 1*. <https://wiki.multimedia.cx/index.php/Sorenson_Video_1>
- Mike Melanson — *VQ Case Study: Sorenson Video 1*. <https://multimedia.cx/eggs/vq-case-study-sorenson-video-1/>
- VideoLAN Wiki — *Sorenson Video*. <https://wiki.videolan.org/Sorenson_Video/>

There is no IETF RFC for SVQ1 and no SDP `a=rtpmap` registration; the
codec was distributed inside QuickTime/MOV containers and never
standardised for streaming.

## License

Licensed under the [MIT License](LICENSE).
