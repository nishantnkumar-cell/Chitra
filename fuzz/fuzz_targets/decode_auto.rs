#![no_main]
//! Decode arbitrary bytes as the format their first bytes say (else every format).

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    chitra::fuzzing::decode(data);
});
