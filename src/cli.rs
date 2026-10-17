//! Command-line interface: `chitra convert`, `chitra info` and `chitra passport`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};

use chitra::convert::{self, ConflictPolicy, ConvertOptions, Outcome};
use chitra::decode::{self, DecodeOptions};
use chitra::format::Format;
use chitra::metadata;
use chitra::passport;
use chitra::resize::ResizeSpec;

pub const USAGE: &str = "\
Chitra - fast photo viewer and batch converter

USAGE:
    chitra [FILE|FOLDER]              Open the viewer
    chitra convert [OPTIONS] INPUT... Convert files or folders
    chitra info FILE...               Show format, size and EXIF
    chitra passport [OPTIONS] FILE    Make a passport or visa photo
    chitra --help | --version

    chitra-cli convert|info|passport  Same, as a console program for scripts
                                     (cmd and PowerShell wait for it)

FORMATS: jpg, jxl (JPEG XL), png, gif, bmp, webp, avif
         (HEIC/HEIF files can be converted from, not to)

CONVERT OPTIONS:
    -t, --to FORMAT          Output format (required)
    -q, --quality N          Quality 1-100 for JPEG, WebP, JPEG XL and AVIF,
                             default 90; for GIF, palette quality
        --lossless           Lossless WebP / JPEG XL
    -s, --resize SIZE        2048 = longest side, 50% = scale, 1920x1080 = fit in box
        --upscale            Allow --resize to enlarge small images
    -o, --out DIR            Output folder (default: next to each input)
    -n, --rename PATTERN     Output name without extension (default {name})
                             Tokens: {name} {parent} {n} {n:03} {ext}
    -r, --recursive          Include sub-folders (mirrored under --out)
        --on-conflict MODE   rename (default), skip or overwrite
        --strip-metadata     Do not copy EXIF / colour profile
        --no-animation       Write only the first frame of animated GIFs
    -j, --jobs N             Parallel workers (default: all cores)
        --dry-run            Show what would be written, change nothing
        --quiet              Only print the summary and errors

NOTES:
    WebP and JPEG XL are written lossy unless --lossless is given (builds
    without the lossy-encoders feature always write them lossless).
    EXIF orientation is applied to the pixels, so outputs always display upright.

PASSPORT OPTIONS:
    -c, --country ID         Photo size: uk, eu, us, india, india-visa,
                             canada, australia, china, japan (--list shows all)
        --crown X,Y          Pixel at the top of the head (required)
        --chin X,Y           Pixel at the bottom of the chin (required)
        --sheet PAPER        Print sheet instead of one photo: 6x4, 15x10,
                             7x5, a4 or letter (300 dpi, with cutting guides)
        --no-straighten      Keep the photo's tilt
    -o, --out FILE           Output JPEG (default: NAME-passport-ID.jpg)
        --list               List the photo sizes

EXAMPLES:
    chitra convert photos/ --to webp --out web/ --resize 2048
    chitra convert *.png --to jpg -q 85 --rename \"holiday_{n:03}\"
    chitra convert scans/ -r --to jxl --out archive/
    chitra passport me.jpg -c uk --crown 1520,610 --chin 1530,1640 --sheet 6x4
";

pub fn version() -> String {
    let lossy = if chitra::encode::LOSSY_ENCODERS {
        " (lossy WebP/JXL encoders)"
    } else {
        ""
    };
    format!("chitra {}{lossy}", env!("CARGO_PKG_VERSION"))
}

fn parse_convert(
    args: Vec<OsString>,
) -> Result<(ConvertOptions, Vec<PathBuf>, bool, bool), lexopt::Error> {
    use lexopt::prelude::*;

    let mut parser = lexopt::Parser::from_args(args);
    let mut format: Option<Format> = None;
    let mut opts = ConvertOptions::new(Format::Png);
    let mut inputs = Vec::new();
    let mut dry_run = false;
    let mut quiet = false;

    while let Some(arg) = parser.next()? {
        match arg {
            Short('t') | Long("to") | Long("format") => {
                format = Some(parser.value()?.parse_with(|s| s.parse::<Format>())?);
            }
            Short('q') | Long("quality") => {
                let q: u8 = parser.value()?.parse()?;
                if !(1..=100).contains(&q) {
                    return Err("quality must be between 1 and 100".into());
                }
                opts.quality = q;
            }
            Short('s') | Long("resize") => {
                opts.resize = Some(parser.value()?.parse_with(|s| s.parse::<ResizeSpec>())?);
            }
            Long("upscale") => opts.upscale = true,
            Long("lossless") => opts.lossless = true,
            Short('o') | Long("out") => opts.out_dir = Some(PathBuf::from(parser.value()?)),
            Short('n') | Long("rename") => opts.rename = parser.value()?.string()?,
            Short('r') | Long("recursive") => opts.recursive = true,
            Long("on-conflict") => {
                opts.conflict = parser
                    .value()?
                    .parse_with(|s| s.parse::<ConflictPolicy>())?;
            }
            Long("strip-metadata") => opts.keep_metadata = false,
            Long("keep-metadata") => opts.keep_metadata = true,
            Long("no-animation") => opts.keep_animation = false,
            Short('j') | Long("jobs") => opts.jobs = parser.value()?.parse()?,
            Long("dry-run") => dry_run = true,
            Long("quiet") => quiet = true,
            Short('h') | Long("help") => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            Value(v) => inputs.push(PathBuf::from(v)),
            _ => return Err(arg.unexpected()),
        }
    }
    opts.format = format.ok_or("missing --to FORMAT (jpg, jxl, png, gif, bmp, webp or avif)")?;
    if inputs.is_empty() {
        return Err("no input files or folders given".into());
    }
    Ok((opts, inputs, dry_run, quiet))
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn run_convert(args: Vec<OsString>) -> ExitCode {
    let (opts, inputs, dry_run, quiet) = match parse_convert(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}\n\nRun 'chitra convert --help' for usage.");
            return ExitCode::from(2);
        }
    };
    for input in &inputs {
        if !input.exists() {
            eprintln!("error: {} does not exist", input.display());
            return ExitCode::from(2);
        }
    }

    let jobs = convert::plan(&inputs, &opts);
    if jobs.is_empty() {
        eprintln!("No supported images found.");
        return ExitCode::from(1);
    }

    if dry_run {
        for job in &jobs {
            match &job.skip {
                Some(reason) => println!("skip  {}  ({reason})", job.input.display()),
                None => println!("{}  ->  {}", job.input.display(), job.output.display()),
            }
        }
        println!("{} file(s) planned, nothing written (dry run).", jobs.len());
        return ExitCode::SUCCESS;
    }

    let total = jobs.len();
    let done = AtomicUsize::new(0);
    let report = convert::run(jobs, &opts, |r| {
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        match &r.outcome {
            Outcome::Converted {
                width,
                height,
                in_bytes,
                out_bytes,
            } => {
                if !quiet {
                    println!(
                        "[{n}/{total}] {} -> {} ({width}x{height}, {} -> {}, {} ms)",
                        r.job.input.display(),
                        r.job.output.display(),
                        human_bytes(*in_bytes),
                        human_bytes(*out_bytes),
                        r.elapsed.as_millis()
                    );
                }
            }
            Outcome::Skipped(why) => {
                if !quiet {
                    println!("[{n}/{total}] skipped {} ({why})", r.job.input.display());
                }
            }
            Outcome::Failed(why) => {
                eprintln!("[{n}/{total}] FAILED {}: {why}", r.job.input.display())
            }
        }
    });

    let (bin, bout) = report.bytes();
    println!(
        "\nDone in {:.2} s: {} converted, {} skipped, {} failed. {} -> {}.",
        report.elapsed.as_secs_f64(),
        report.converted(),
        report.skipped(),
        report.failed(),
        human_bytes(bin),
        human_bytes(bout)
    );
    if report.failed() > 0 {
        eprintln!("\nFailures:");
        for r in &report.results {
            if let Outcome::Failed(why) = &r.outcome {
                eprintln!("  {}: {why}", r.job.input.display());
            }
        }
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn parse_point(s: &str) -> Result<(f32, f32), String> {
    let bad = || format!("invalid point '{s}' (use X,Y in pixels)");
    let (x, y) = s.split_once(',').ok_or_else(bad)?;
    let x: f32 = x.trim().parse().map_err(|_| bad())?;
    let y: f32 = y.trim().parse().map_err(|_| bad())?;
    if !(x.is_finite() && y.is_finite()) {
        return Err(bad());
    }
    Ok((x, y))
}

struct PassportArgs {
    input: PathBuf,
    spec: &'static passport::PhotoSpec,
    marks: passport::Marks,
    sheet: Option<passport::Paper>,
    straighten: bool,
    out: Option<PathBuf>,
}

fn parse_passport(args: Vec<OsString>) -> Result<Option<PassportArgs>, lexopt::Error> {
    use lexopt::prelude::*;

    let mut parser = lexopt::Parser::from_args(args);
    let mut input = None;
    let mut spec = None;
    let mut crown = None;
    let mut chin = None;
    let mut sheet = None;
    let mut straighten = true;
    let mut out = None;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('c') | Long("country") => {
                let id = parser.value()?.string()?;
                spec = Some(passport::spec(&id).ok_or_else(|| {
                    format!("unknown country '{id}'; run 'chitra passport --list'")
                })?);
            }
            Long("crown") => crown = Some(parser.value()?.parse_with(parse_point)?),
            Long("chin") => chin = Some(parser.value()?.parse_with(parse_point)?),
            Long("sheet") => {
                sheet = Some(
                    parser
                        .value()?
                        .parse_with(|s| s.parse::<passport::Paper>())?,
                )
            }
            Long("no-straighten") => straighten = false,
            Short('o') | Long("out") => out = Some(PathBuf::from(parser.value()?)),
            Long("list") => {
                for s in passport::SPECS {
                    println!(
                        "{:<11} {:<30} {}, head {}-{} mm, {}",
                        s.id,
                        s.name,
                        s.size_label(),
                        s.head_mm.0,
                        s.head_mm.1,
                        s.background
                    );
                }
                return Ok(None);
            }
            Short('h') | Long("help") => {
                print!("{USAGE}");
                return Ok(None);
            }
            Value(v) if input.is_none() => input = Some(PathBuf::from(v)),
            _ => return Err(arg.unexpected()),
        }
    }
    let marks = passport::Marks {
        crown: crown.ok_or("missing --crown X,Y (the top of the head)")?,
        chin: chin.ok_or("missing --chin X,Y (the bottom of the chin)")?,
    };
    if !marks.valid() {
        return Err("--chin must be below --crown".into());
    }
    Ok(Some(PassportArgs {
        input: input.ok_or("no input photo given")?,
        spec: spec.ok_or("missing --country (run 'chitra passport --list')")?,
        marks,
        sheet,
        straighten,
        out,
    }))
}

pub fn run_passport(args: Vec<OsString>) -> ExitCode {
    let a = match parse_passport(args) {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}\n\nRun 'chitra passport --help' for usage.");
            return ExitCode::from(2);
        }
    };
    let decoded = match decode::decode_file(&a.input, DecodeOptions::default()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {}: {e}", a.input.display());
            return ExitCode::from(1);
        }
    };
    let mut layout = passport::Layout::new(a.spec, a.marks);
    layout.straighten = a.straighten;
    let (image, dpi, copies) = match a.sheet {
        Some(paper) => {
            let opts = passport::SheetOptions::new(paper);
            let (w, h) = a.spec.pixels_at(opts.dpi);
            let photo = passport::render(&decoded.image, a.spec, &layout, w, h);
            let (sheet, n) = passport::print_sheet(&photo, a.spec, &opts);
            if n == 0 {
                eprintln!(
                    "error: a {} photo does not fit on {}",
                    a.spec.size_label(),
                    paper.name()
                );
                return ExitCode::from(1);
            }
            (sheet, opts.dpi, n)
        }
        None => {
            let (w, h) = a.spec.single_pixels();
            let photo = passport::render(&decoded.image, a.spec, &layout, w, h);
            (photo, passport::dpi_of(h, a.spec.height_mm), 1)
        }
    };
    let out = a.out.unwrap_or_else(|| {
        let stem = a.input.file_stem().unwrap_or_default().to_string_lossy();
        let kind = if a.sheet.is_some() {
            "sheet"
        } else {
            "passport"
        };
        a.input
            .with_file_name(format!("{stem}-{kind}-{}.jpg", a.spec.id))
    });
    let mut buf = Vec::new();
    let result = passport::write_jpeg(&image, dpi, &mut buf)
        .map_err(|e| e.to_string())
        .and_then(|()| std::fs::write(&out, buf).map_err(|e| e.to_string()));
    match result {
        Ok(()) => {
            println!(
                "{} -> {} ({}x{}, {} cop{}, {}, {:.0} dpi)",
                a.input.display(),
                out.display(),
                image.width(),
                image.height(),
                copies,
                if copies == 1 { "y" } else { "ies" },
                a.spec.name,
                dpi
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: cannot write {}: {e}", out.display());
            ExitCode::from(1)
        }
    }
}

pub fn run_info(args: Vec<OsString>) -> ExitCode {
    if args.is_empty() {
        eprintln!("usage: chitra info FILE...");
        return ExitCode::from(2);
    }
    let mut failed = false;
    for arg in args {
        let path = PathBuf::from(arg);
        println!("{}", path.display());
        match decode::decode_file(&path, DecodeOptions { animation: true }) {
            Ok(d) => {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                println!("  Format:     {}", d.format);
                println!("  Dimensions: {} x {}", d.image.width(), d.image.height());
                println!("  Pixels:     {:?}", d.image.color());
                println!("  File size:  {}", human_bytes(size));
                if d.is_animated() {
                    let ms: u128 = d.frames.iter().map(|f| f.delay.as_millis()).sum();
                    println!(
                        "  Animation:  {} frames, {:.2} s",
                        d.frames.len(),
                        ms as f64 / 1000.0
                    );
                }
                if d.icc.is_some() {
                    println!("  ICC:        embedded profile");
                }
                if let Some(exif) = &d.exif {
                    for (k, v) in metadata::readable_fields(exif) {
                        println!("  {k:<11} {v}");
                    }
                }
            }
            Err(e) => {
                println!("  error: {e}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
