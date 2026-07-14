//! Fuzz the full SVQ1 frame decode entry point with no reference
//! frame: arbitrary bytes must either decode into a well-formed
//! `Svq1DecodedFrame` or fail with a structured `Error` — never
//! panic, never overrun a canvas.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_svq::svq1_plane::{chroma_dim, decode_frame};

fuzz_target!(|data: &[u8]| {
    if let Ok(frame) = decode_frame(data, None) {
        // Structural invariants of a successful decode: canvas
        // geometry matches the declared header dimensions and the
        // 4:1:0 chroma subsample (spec/02 §2.2 / §2.3).
        assert_eq!(frame.y.samples.len(), frame.y.stride * frame.y.rows);
        assert_eq!(frame.u.width, chroma_dim(frame.width()));
        assert_eq!(frame.u.height, chroma_dim(frame.height()));
        assert_eq!(frame.v.width, frame.u.width);
        assert_eq!(frame.v.height, frame.u.height);
    }
});
