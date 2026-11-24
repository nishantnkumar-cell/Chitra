//! Corrupt and hostile input must be refused, never crash a parser. This is a
//! quick deterministic cousin of the fuzz targets in `fuzz/`: it mutates real
//! files in every supported format (bit flips, cuts, inserted garbage) and
//! feeds random bytes and hostile patterns to the parsers, with the app's panic
//! net removed (`chitra::fuzzing`), so a panic here is a real bug.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use chitra::encode::{EncodeOptions, encode};
use chitra::format::Format;
use chitra::fuzzing;
use common::*;

/// A small deterministic random source (xorshift64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    match rng.below(4) {
        0 => {
            // Flip a few bits.
            for _ in 0..1 + rng.below(4) {
                if !v.is_empty() {
                    let i = rng.below(v.len());
                    v[i] ^= 1 << rng.below(8);
                }
            }
        }
        1 => v.truncate(rng.below(v.len() + 1)),
        2 => {
            // Overwrite a run with random bytes.
            if !v.is_empty() {
                let at = rng.below(v.len());
                let n = (1 + rng.below(16)).min(v.len() - at);
                v[at..at + n].copy_from_slice(&rng.bytes(n));
            }
        }
        _ => {
            // Insert garbage.
            let at = rng.below(v.len() + 1);
            let n = 1 + rng.below(32);
            let junk = rng.bytes(n);
            v.splice(at..at, junk);
        }
    }
    v
}

/// Runs `f`, and records a panic with what was being tried.
fn guard(failures: &mut Vec<String>, what: &str, f: impl FnOnce()) {
    static QUIET: std::sync::Once = std::sync::Once::new();
    QUIET.call_once(|| std::panic::set_hook(Box::new(|_| {})));
    if let Err(e) = catch_unwind(AssertUnwindSafe(f)) {
        let msg = e
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        failures.push(format!("{what}: {msg}"));
    }
}

fn seeds() -> Vec<(String, Format, Vec<u8>)> {
    let img = pattern_rgb(24, 16);
    let mut out = Vec::new();
    for f in Format::WRITABLE {
        let mut buf = Vec::new();
        encode(&img, &[], &EncodeOptions::new(f), &mut buf).unwrap();
        out.push((format!("{f:?}"), f, buf));
    }
    for (name, f) in [
        ("pattern.avif", Format::Avif),
        ("pattern.heic", Format::Heif),
        ("alpha.heic", Format::Heif),
        ("exif.jxl", Format::JpegXl),
        ("animated5.jxl", Format::JpegXl),
    ] {
        let data = std::fs::read(std::path::Path::new("tests/data").join(name)).unwrap();
        out.push((name.to_string(), f, data));
    }
    out
}

#[test]
fn mutated_images_never_panic_a_decoder() {
    let mut failures = Vec::new();
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    for (name, format, seed) in seeds() {
        guard(&mut failures, &format!("{name} (unmutated)"), || {
            fuzzing::decode_as(&seed, format)
        });
        // The slow decoders get fewer tries.
        let tries = if matches!(format, Format::Avif | Format::Heif) {
            10
        } else {
            40
        };
        for i in 0..tries {
            let data = mutate(&mut rng, &seed);
            guard(&mut failures, &format!("{name} mutation {i}"), || {
                fuzzing::decode_as(&data, format)
            });
        }
    }
    assert!(
        failures.is_empty(),
        "{} panics: {failures:#?}",
        failures.len()
    );
}

#[test]
fn random_bytes_never_panic_any_parser() {
    let mut failures = Vec::new();
    let mut rng = Rng(0xdead_beef_0000_0002);
    for i in 0..300 {
        let n = rng.below(2048);
        let data = rng.bytes(n);
        guard(
            &mut failures,
            &format!("random {i} ({} bytes)", data.len()),
            || {
                fuzzing::decode(&data);
                fuzzing::pth(&data);
                fuzzing::release_json(&data);
                fuzzing::rename(&data);
                fuzzing::exif(&data);
            },
        );
    }
    assert!(
        failures.is_empty(),
        "{} panics: {failures:#?}",
        failures.len()
    );
}

/// A stored (uncompressed) zip with one file, as PyTorch writes its weights.
fn zip_with(name: &str, data: &[u8]) -> Vec<u8> {
    let mut z = Vec::new();
    let crc = 0u32; // not checked by the reader
    z.extend(b"PK\x03\x04");
    z.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend(crc.to_le_bytes());
    z.extend((data.len() as u32).to_le_bytes());
    z.extend((data.len() as u32).to_le_bytes());
    z.extend((name.len() as u16).to_le_bytes());
    z.extend([0, 0]);
    z.extend(name.as_bytes());
    z.extend(data);
    let cd = z.len() as u32;
    z.extend(b"PK\x01\x02");
    z.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend(crc.to_le_bytes());
    z.extend((data.len() as u32).to_le_bytes());
    z.extend((data.len() as u32).to_le_bytes());
    z.extend((name.len() as u16).to_le_bytes());
    z.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend(name.as_bytes());
    let end = z.len() as u32 - cd;
    z.extend(b"PK\x05\x06");
    z.extend([0, 0, 0, 0, 1, 0, 1, 0]);
    z.extend(end.to_le_bytes());
    z.extend(cd.to_le_bytes());
    z.extend([0, 0]);
    z
}

#[test]
fn hostile_models_json_patterns_and_exif_are_refused() {
    let mut failures = Vec::new();
    let mut rng = Rng(0x0bad_cafe_0000_0003);

    // Weights: a zip around a pickle, mutated; pickles of every opcode byte.
    let seed = zip_with("m/data.pkl", b"\x80\x02}q\x00.");
    for i in 0..400 {
        let data = mutate(&mut rng, &seed);
        guard(&mut failures, &format!("weights mutation {i}"), || {
            fuzzing::pth(&data)
        });
    }
    for op in 0..=255u8 {
        let mut pkl = vec![0x80, 0x02, op];
        pkl.extend(rng.bytes(12));
        let z = zip_with("m/data.pkl", &pkl);
        guard(&mut failures, &format!("pickle opcode {op:#x}"), || {
            fuzzing::pth(&z)
        });
    }

    // JSON: nesting bombs and junk must not overflow the stack.
    for json in [
        "[".repeat(200_000),
        "{\"a\":".repeat(100_000),
        format!("{}1{}", "[".repeat(5000), "]".repeat(5000)),
        "\"\\u12".to_string(),
        "{\"tag_name\": \"v9\", \"assets\": [[[[[]]]]]}".to_string(),
        "1e999999999999".to_string(),
    ] {
        guard(&mut failures, "json", || {
            fuzzing::release_json(json.as_bytes())
        });
    }

    // Rename patterns.
    for pat in [
        "{n:99999999999999999999999999}",
        "{n:",
        "{",
        "}{}{",
        "{name}{name}{name}{parent}{ext}",
        "../../etc/passwd",
        "..\\..\\x",
        "con",
        "\u{0}\u{1}\u{202e}",
        &"{n:5}".repeat(10_000),
    ] {
        guard(&mut failures, pat, || fuzzing::rename(pat.as_bytes()));
    }

    // EXIF: a block with a location, mutated (offsets and counts go wrong).
    use exif::experimental::Writer;
    use exif::{Field, In, Rational, Tag, Value};
    let mut w = Writer::new();
    let fields = [
        Field {
            tag: Tag::Make,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"Cam".to_vec()]),
        },
        Field {
            tag: Tag::GPSLatitude,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![Rational { num: 1, denom: 1 }; 3]),
        },
    ];
    for f in &fields {
        w.push_field(f);
    }
    for little in [true, false] {
        let mut buf = std::io::Cursor::new(Vec::new());
        w.write(&mut buf, little).unwrap();
        let seed = buf.into_inner();
        for i in 0..800 {
            let data = mutate(&mut rng, &seed);
            guard(
                &mut failures,
                &format!("exif mutation {i} little={little}"),
                || fuzzing::exif(&data),
            );
        }
    }
    assert!(
        failures.is_empty(),
        "{} panics: {failures:#?}",
        failures.len()
    );
}
