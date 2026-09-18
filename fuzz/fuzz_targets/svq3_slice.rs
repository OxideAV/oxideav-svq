//! Fuzz the SVQ3 slice envelope: the wire-slice prefix/size parse,
//! the byte-permutation reversal (`unpermute_slice_payload`), and the
//! slice-header field walk (spec/07 §2–§3, including the extended-mode
//! read sequence of §3.3) on arbitrary bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_svq::svq3::{
    parse_slice_header, parse_wire_slice, unpermute_slice_payload, SliceVersion,
};

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let num_mbs = 1 + (data[0] as u32) * 4;
    let protected = data[1] & 1 != 0;
    let wire = &data[2..];

    let extended_mode = data[1] & 4 != 0;
    if let Ok(slice) = parse_wire_slice(wire, num_mbs, protected, extended_mode) {
        // The unpermuted payload is exactly the declared body size and
        // the header ends inside it.
        assert_eq!(slice.payload.len(), slice.header.slice_size as usize);
        assert!(slice.header.header_end_bit <= slice.payload.len() * 8);
        assert!(slice.consumed <= wire.len());
    }

    // Drive the permutation reversal directly across all legal
    // slice-size-size values too (parse_wire_slice only reaches it
    // through a well-formed prefix).
    let sss = (1 + (data[1] >> 6)).min(3); // 1..=3
    let _ = unpermute_slice_payload(wire, sss);

    // And the slice-header field walk directly on arbitrary
    // "already-unpermuted" bytes, sweeping both header versions and
    // the protected flag independent of envelope validity (the V2
    // arm reads a num_mbs-derived mb-offset width).
    let version = if data[1] & 2 != 0 {
        SliceVersion::V2
    } else {
        SliceVersion::V1
    };
    let _ = parse_slice_header(
        wire,
        version,
        sss,
        wire.len() as u32,
        num_mbs,
        protected,
        extended_mode,
    );
});
