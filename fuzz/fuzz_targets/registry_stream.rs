//! Fuzz the framework-facing decoder handles end-to-end: the
//! `make_decoder` (SVQ1) / `make_svq3_decoder` (SVQ3) factories, the
//! `send_packet` → `receive_frame` protocol, multi-packet reference
//! chaining, and the SVQ3 extradata-at-construction path. Arbitrary
//! packetisations of arbitrary bytes must never panic the handles or
//! wedge the protocol (every `send_packet` is followed by a
//! `receive_frame` drain).

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_core::{CodecId, CodecParameters, Decoder, Packet, TimeBase};
use oxideav_svq::{make_decoder, make_svq3_decoder, CODEC_ID_STR, SVQ3_CODEC_ID_STR};

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let use_svq3 = data[0] & 1 != 0;
    let n_packets = 1 + (data[1] as usize % 4);
    let body = &data[2..];

    let mut decoder: Box<dyn Decoder> = if use_svq3 {
        // First half of the body doubles as candidate extradata —
        // exercises the eager SEQH parse at construction (missing /
        // unparseable extradata must not be fatal there).
        let mut params = CodecParameters::video(CodecId::new(SVQ3_CODEC_ID_STR));
        params.extradata = body[..body.len() / 2].to_vec();
        match make_svq3_decoder(&params) {
            Ok(d) => d,
            Err(_) => return,
        }
    } else {
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        match make_decoder(&params) {
            Ok(d) => d,
            Err(_) => return,
        }
    };

    // Split the body into n roughly-equal packets and run the
    // protocol: send, then drain receive_frame until it errors
    // (NeedMore / Eof / a structured decode error).
    let chunk = (body.len() / n_packets).max(1);
    for part in body.chunks(chunk).take(n_packets) {
        let packet = Packet::new(0, TimeBase::MICROS, part.to_vec());
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame().is_ok() {}
    }
    let _ = decoder.flush();
    while decoder.receive_frame().is_ok() {}
});
