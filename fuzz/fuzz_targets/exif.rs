#![no_main]
//! Read arbitrary bytes as an EXIF block and remove its location.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    chitra::fuzzing::exif(data);
});
