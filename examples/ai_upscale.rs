//! Upscale one photo with an AI model and print the time:
//! `cargo run --release --example ai_upscale -- in.png out.png [MODEL] [NOISE 0-1]`.
//! MODEL is fast (default), best, best-2x, faithful or drawings; it must be
//! installed (see `chitra-cli ai-model download`).

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: ai_upscale IN OUT [MODEL] [NOISE]");
        std::process::exit(2);
    }
    let model = args
        .get(3)
        .map(|m| chitra::ai::Model::from_id(m).expect("unknown model"))
        .unwrap_or_default();
    let noise: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let img = image::open(&args[1]).expect("open");
    let t = std::time::Instant::now();
    let out = chitra::ai::upscale(&img, model, noise).expect("upscale");
    eprintln!(
        "{}x{} -> {}x{} in {:.2} s on the {}",
        img.width(),
        img.height(),
        out.width(),
        out.height(),
        t.elapsed().as_secs_f64(),
        chitra::ai::last_device()
    );
    out.save(&args[2]).expect("save");
}
