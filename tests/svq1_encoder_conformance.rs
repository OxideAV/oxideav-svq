//! SVQ1 encoder conformance — black-box cross-validation fixtures.
//!
//! `tests/fixtures/encoded_176x144_stage.svq1` is OUR encoder's
//! deterministic output for the synthetic gradient below in the
//! `MeanPlusOneStageL3` mode.
//! `tests/fixtures/encoded_176x144_stage.yuv410p` is the reference
//! decoder binary's decode of that stream (black-box only; the
//! stream was wrapped in a minimal container for delivery), which
//! matched OUR decoder's output byte-exact at fixture-generation
//! time — the "official decoder accepts the naïve stream" milestone
//! the staged encoder companion
//! (`docs/video/svq1/wiki/eggs-naive-svq1-encoder.html`) describes.
//! The same cross-check passed for the `MeanOnlyL5` and `MeanOnlyL3`
//! modes.
//!
//! The tests pin (1) encoder determinism against the committed
//! stream and (2) our decode of that stream against the committed
//! reference decode — so a regression on EITHER side of the wire
//! breaks CI.

use oxideav_svq::svq1_enc::{encode_intra_frame, Svq1EncoderMode, Svq1PlaneRef};
use oxideav_svq::svq1_plane::{chroma_dim, decode_intra_frame};

const ENCODED: &[u8] = include_bytes!("fixtures/encoded_176x144_stage.svq1");
const REF_YUV: &[u8] = include_bytes!("fixtures/encoded_176x144_stage.yuv410p");

const W: usize = 176;
const H: usize = 144;

fn gradient_plane(width: usize, height: usize, seed: u32) -> Vec<u8> {
    (0..width * height)
        .map(|i| {
            let x = (i % width) as u32;
            let y = (i / width) as u32;
            ((x * 3 + y * 5 + seed) % 256) as u8
        })
        .collect()
}

#[test]
fn encoder_output_matches_committed_stream_and_reference_decode() {
    let (cw, ch) = (chroma_dim(W), chroma_dim(H));
    let y = gradient_plane(W, H, 7);
    let u = gradient_plane(cw, ch, 101);
    let v = gradient_plane(cw, ch, 202);

    let encoded = encode_intra_frame(
        Svq1PlaneRef {
            samples: &y,
            width: W,
            height: H,
        },
        Svq1PlaneRef {
            samples: &u,
            width: cw,
            height: ch,
        },
        Svq1PlaneRef {
            samples: &v,
            width: cw,
            height: ch,
        },
        Svq1EncoderMode::MeanPlusOneStageL3,
    )
    .expect("encodes");
    assert_eq!(encoded, ENCODED, "encoder output must be byte-stable");

    // Our decode of the committed stream equals the reference
    // binary's decode of the same stream.
    let frame = decode_intra_frame(ENCODED).expect("decodes");
    let mut ours = frame.y.visible();
    ours.extend(frame.u.visible());
    ours.extend(frame.v.visible());
    assert_eq!(
        ours, REF_YUV,
        "our decode must equal the reference decode of our own stream"
    );
}
