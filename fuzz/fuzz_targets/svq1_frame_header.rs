//! Fuzz the SVQ1 frame-header parser (`parse_frame_header`) on
//! arbitrary bytes: it must never panic, only return
//! `Ok(Svq1FrameHeader)` or a structured `Error`.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(header) = oxideav_svq::parse_frame_header(data) {
        // A successful parse must be internally consistent: explicit
        // dimensions live in a 12-bit domain (spec/02 §2.1) and the
        // reported header end must lie inside the input bit span.
        if let Some(width) = header.width {
            assert!(width <= 4095, "width out of 12-bit domain");
        }
        if let Some(height) = header.height {
            assert!(height <= 4095, "height out of 12-bit domain");
        }
        assert!(
            header.header_end_bit <= data.len() * 8,
            "header end past input"
        );
    }
});
