#![no_main]
//! Run arbitrary text through the rename pattern engine.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    chitra::fuzzing::rename(data);
});
