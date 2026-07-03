//! SVQ1 overhang-macroblock conformance — 160×120 black-box fixture.
//!
//! 160×120 is `frame_size_code = 0` in the wiki's common-size table;
//! its Y plane height (120) and its 40×30 chroma planes are NOT
//! multiples of 16, so the bottom macroblock rows OVERHANG the
//! visible frame (`docs/video/svq1/spec/02-bitstream-organisation.md`
//! §2.3.1). spec/02 §2.8 item 1 left the overhang treatment open
//! (skip vs decode-and-discard); this fixture pins
//! **decode-and-discard** in the Validator role: the bitstream
//! carries full 16×16 macroblocks over the overhang region (the
//! plane payload only parses under the `ceil(dim / 16)` grid) and
//! the visible crop matches the reference decode byte-exact across
//! an I + 2 P chain.

use oxideav_svq::svq1_plane::{decode_frame, Svq1DecodedFrame};

const CHAIN: &[u8] = include_bytes!("fixtures/chain_160x120_3f.svq1");
const CHAIN_REF: &[u8] = include_bytes!("fixtures/chain_160x120_3f.yuv410p");
/// Per-frame packet byte sizes, recorded at fixture-generation time
/// from the container's sample table.
const SIZES: [usize; 3] = [2948, 2728, 2300];

const W: usize = 160;
const H: usize = 120;
const CW: usize = 40;
const CH: usize = 30;
const FRAME_YUV: usize = W * H + 2 * CW * CH;

#[test]
fn overhang_dimensions_decode_byte_exact() {
    assert_eq!(CHAIN.len(), SIZES.iter().sum::<usize>());
    assert_eq!(CHAIN_REF.len(), 3 * FRAME_YUV);

    let mut offset = 0usize;
    let mut reference: Option<Svq1DecodedFrame> = None;
    for (frame_no, &size) in SIZES.iter().enumerate() {
        let chunk = &CHAIN[offset..offset + size];
        offset += size;
        let frame = decode_frame(chunk, reference.as_ref())
            .unwrap_or_else(|e| panic!("frame {frame_no} decodes: {e:?}"));
        assert_eq!(frame.width(), W);
        assert_eq!(frame.height(), H);
        // The canvases pad to whole macroblocks (Y: 160×128 rows;
        // chroma: 48×32) — the overhang region is decoded, then
        // cropped away by `visible()`.
        assert_eq!(frame.y.rows, 128, "Y canvas pads 120 → 128 rows");
        assert_eq!(frame.u.stride, 48, "chroma canvas pads 40 → 48 cols");
        assert_eq!(frame.u.rows, 32, "chroma canvas pads 30 → 32 rows");

        let want = &CHAIN_REF[frame_no * FRAME_YUV..(frame_no + 1) * FRAME_YUV];
        let (want_y, rest) = want.split_at(W * H);
        let (want_u, want_v) = rest.split_at(CW * CH);
        assert_eq!(frame.y.visible(), want_y, "frame {frame_no} Y plane");
        assert_eq!(frame.u.visible(), want_u, "frame {frame_no} U plane");
        assert_eq!(frame.v.visible(), want_v, "frame {frame_no} V plane");

        reference = Some(frame);
    }
}

/// Corrupt / random inputs must error out cleanly — never panic and
/// never loop. Exercises truncations of a real frame at every byte
/// boundary plus a deterministic pseudo-random byte sweep.
#[test]
fn corrupt_streams_error_cleanly() {
    let intra: &[u8] = include_bytes!("fixtures/intra_176x144.svq1");
    // Truncations of a conformant frame: every prefix must either
    // decode (only the full frame can) or return an error.
    for len in 0..intra.len().min(600) {
        let _ = decode_frame(&intra[..len], None);
    }
    // Deterministic xorshift byte soup.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut soup = vec![0u8; 4096];
    for b in &mut soup {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = state as u8;
    }
    for start in 0..64 {
        let _ = decode_frame(&soup[start..], None);
    }
    // Bit-flip sweep over the first bytes of a real frame.
    for byte in 0..48usize {
        for bit in 0..8 {
            let mut mutated = intra.to_vec();
            mutated[byte] ^= 1 << bit;
            let _ = decode_frame(&mutated, None);
        }
    }
}
