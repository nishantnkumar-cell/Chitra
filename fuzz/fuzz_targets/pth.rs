#![no_main]
//! Parse arbitrary bytes as PyTorch model weights.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    chitra::fuzzing::pth(data);
});
