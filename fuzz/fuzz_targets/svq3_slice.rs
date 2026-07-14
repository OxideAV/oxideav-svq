//! Fuzz the SVQ3 slice envelope: the wire-slice prefix/size parse,
//! the byte-permutation reversal (`unpermute_slice_payload`), and the
//! slice-header field walk (wiki §"Slice Header") on arbitrary bytes.

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

    if let Ok((header, payload)) = parse_wire_slice(wire, num_mbs, protected) {
        // The returned macroblock-layer bytes are the slice body past
        // the parsed header — never more than the declared body size.
        assert!(payload.len() <= header.slice_size as usize);
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
    let _ = parse_slice_header(wire, version, sss, wire.len() as u32, num_mbs, protected);
});
