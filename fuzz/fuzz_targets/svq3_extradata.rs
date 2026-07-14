//! Fuzz the SVQ3 sequence-header (extradata) parse layer: the `SEQH`
//! prefix strip and the sequence-header field walk (wiki §"Sequence
//! Header") must never panic on arbitrary bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_svq::svq3::{num_macroblocks, parse_extradata, parse_sequence_header};

fuzz_target!(|data: &[u8]| {
    let _ = parse_extradata(data);
    if let Ok(seqh) = parse_sequence_header(data) {
        // Derived geometry must stay well-defined for any accepted
        // header (12-bit dimension domain).
        let _ = num_macroblocks(&seqh);
    }
});
