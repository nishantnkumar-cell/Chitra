//! Times decoding (and the viewer's screen-size downscale) for the files given.
//! `cargo run --release --example decode_bench -- photo.jpg photo.jxl`

use std::time::Instant;

use chitra::decode::{DecodeOptions, decode_file};
use chitra::resize;

fn main() {
    for path in std::env::args_os().skip(1) {
        let path = std::path::PathBuf::from(path);
        let t = Instant::now();
        let d = match decode_file(&path, DecodeOptions::default()) {
            Ok(d) => d,
            Err(e) => {
                println!("{}: {e}", path.display());
                continue;
            }
        };
        let decode = t.elapsed();
        let (w, h) = resize::fit_within(d.image.width(), d.image.height(), 2560, 2560);
        let t2 = Instant::now();
        let small = resize::resize(&d.image, w, h).into_rgba8();
        println!(
            "{:<28} {:>5}x{:<5} decode {:>5} ms, fit to 2560 {:>4} ms ({} bytes)",
            path.file_name().unwrap().to_string_lossy(),
            d.image.width(),
            d.image.height(),
            decode.as_millis(),
            t2.elapsed().as_millis(),
            small.len()
        );
    }
}
