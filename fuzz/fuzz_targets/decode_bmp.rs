#![no_main]
//! Decode arbitrary bytes as Bmp, as a still and as an animation.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    chitra::fuzzing::decode_as(data, chitra::format::Format::Bmp);
});
