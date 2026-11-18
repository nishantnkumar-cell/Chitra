//! Upscale one photo 4x with the AI model: `cargo run --release --example
//! ai_upscale -- in.png out.png [noise 0-1]`. Needs the model files (see
//! `chitra-cli ai-model download`).

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: ai_upscale IN OUT [NOISE]");
        std::process::exit(2);
    }
    let noise: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let img = image::open(&args[1]).expect("open");
    let t = std::time::Instant::now();
    let out = chitra::ai::upscale(&img, noise).expect("upscale");
    eprintln!(
        "{}x{} -> {}x{} in {:.2} s",
        img.width(),
        img.height(),
        out.width(),
        out.height(),
        t.elapsed().as_secs_f64()
    );
    out.save(&args[2]).expect("save");
}
