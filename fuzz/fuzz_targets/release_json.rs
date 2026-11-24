#![no_main]
//! Parse arbitrary text as the JSON of a release listing.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    chitra::fuzzing::release_json(data);
});
