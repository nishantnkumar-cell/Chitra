//! Batch conversion: naming, conflicts, folder structure and the CLI.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chitra::convert::{ConflictPolicy, ConvertOptions, Outcome, plan, run};
use chitra::decode::{DecodeOptions, decode_file};
use chitra::encode::{EncodeOptions, encode};
use chitra::format::Format;
use chitra::metadata;
use chitra::resize::ResizeSpec;
use common::*;

fn write_image(path: &Path, format: Format, w: u32, h: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut opts = EncodeOptions::new(format);
    opts.lossless = true;
    let mut f = fs::File::create(path).unwrap();
    encode(&pattern_rgb(w, h), &[], &opts, &mut f).unwrap();
}

#[test]
fn converts_every_format_to_every_format() {
    let dir = tempfile::tempdir().unwrap();
    let inputs: Vec<PathBuf> = Format::WRITABLE
        .iter()
        .map(|f| {
            let p = dir
                .path()
                .join(format!("src_{}.{}", f.extension(), f.extension()));
            write_image(&p, *f, 50, 30);
            p
        })
        .collect();
    for to in Format::WRITABLE {
        let mut opts = ConvertOptions::new(to);
        opts.out_dir = Some(dir.path().join(format!("out_{}", to.extension())));
        let jobs = plan(&inputs, &opts);
        assert_eq!(jobs.len(), Format::WRITABLE.len());
        let report = run(jobs, &opts, |_| {});
        assert_eq!(report.failed(), 0, "to {to:?}: {:?}", report.results);
        for r in &report.results {
            let d = decode_file(&r.job.output, DecodeOptions::default()).unwrap();
            assert_eq!(d.format, to);
            assert_eq!((d.image.width(), d.image.height()), (50, 30));
        }
    }
}

#[test]
fn rename_resize_and_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let a_png = dir.path().join("a.png");
    let a_jpg = dir.path().join("a.jpg");
    write_image(&a_png, Format::Png, 400, 200);
    write_image(&a_jpg, Format::Jpeg, 400, 200);

    // Two inputs that map to the same output name never overwrite each other.
    let mut opts = ConvertOptions::new(Format::WebP);
    opts.resize = Some(ResizeSpec::LongestSide(100));
    let jobs = plan(&[dir.path().to_path_buf()], &opts);
    let names: Vec<String> = jobs
        .iter()
        .map(|j| j.output.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["a.webp", "a_1.webp"]);
    let report = run(jobs, &opts, |_| {});
    assert_eq!(report.converted(), 2);
    let out = decode_file(&dir.path().join("a.webp"), DecodeOptions::default()).unwrap();
    assert_eq!((out.image.width(), out.image.height()), (100, 50));

    // Existing outputs: rename adds a number, skip skips, overwrite reuses the name.
    let mut opts = ConvertOptions::new(Format::WebP);
    let only_png = [a_png.clone()];
    assert!(plan(&only_png, &opts)[0].output.ends_with("a_2.webp"));
    opts.conflict = ConflictPolicy::Skip;
    assert!(plan(&only_png, &opts)[0].skip.is_some());
    opts.conflict = ConflictPolicy::Overwrite;
    let jobs = plan(&only_png, &opts);
    assert!(jobs[0].output.ends_with("a.webp") && jobs[0].skip.is_none());

    // Rename pattern with a counter.
    let mut opts = ConvertOptions::new(Format::Png);
    opts.rename = "photo_{n:03}".into();
    opts.out_dir = Some(dir.path().join("renamed"));
    let report = run(plan(&[a_jpg.clone(), a_png.clone()], &opts), &opts, |_| {});
    assert_eq!(report.converted(), 2);
    assert!(dir.path().join("renamed/photo_001.png").exists());
    assert!(dir.path().join("renamed/photo_002.png").exists());
}

#[test]
fn recursive_mirrors_folders_and_reports_failures() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    write_image(&src.join("top.png"), Format::Png, 10, 10);
    write_image(&src.join("sub/deeper/inner.bmp"), Format::Bmp, 10, 10);
    fs::write(src.join("sub/broken.jpg"), b"not really a jpeg").unwrap();
    fs::write(src.join("notes.txt"), b"ignored").unwrap();

    let mut opts = ConvertOptions::new(Format::JpegXl);
    opts.recursive = true;
    opts.out_dir = Some(dir.path().join("out"));
    let jobs = plan(std::slice::from_ref(&src), &opts);
    assert_eq!(jobs.len(), 3);
    let report = run(jobs, &opts, |_| {});
    assert_eq!(report.converted(), 2);
    assert_eq!(report.failed(), 1);
    assert!(dir.path().join("out/top.jxl").exists());
    assert!(dir.path().join("out/sub/deeper/inner.jxl").exists());
    let failed = report
        .results
        .iter()
        .find(|r| matches!(r.outcome, Outcome::Failed(_)))
        .unwrap();
    assert!(failed.job.input.ends_with("broken.jpg"));
    // A failed conversion leaves no partial or temporary file behind.
    let leftovers: Vec<_> = fs::read_dir(dir.path().join("out/sub"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_type().unwrap().is_file())
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn metadata_is_kept_upright_or_stripped() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("rotated.jpg");
    let mut enc = EncodeOptions::new(Format::Jpeg);
    enc.exif = Some(exif_with_orientation(6));
    let mut f = fs::File::create(&input).unwrap();
    encode(&pattern_rgb(40, 20), &[], &enc, &mut f).unwrap();
    drop(f);

    let mut opts = ConvertOptions::new(Format::Png);
    opts.out_dir = Some(dir.path().join("keep"));
    run(plan(std::slice::from_ref(&input), &opts), &opts, |_| {});
    let kept = decode_file(
        &dir.path().join("keep/rotated.png"),
        DecodeOptions::default(),
    )
    .unwrap();
    // Pixels are rotated once; the copied EXIF says "normal" so viewers don't rotate again.
    assert_eq!((kept.image.width(), kept.image.height()), (20, 40));
    let exif = kept.exif.expect("EXIF copied");
    assert_eq!(
        metadata::orientation(&exif),
        image::metadata::Orientation::NoTransforms
    );

    opts.keep_metadata = false;
    opts.out_dir = Some(dir.path().join("strip"));
    run(plan(&[input], &opts), &opts, |_| {});
    let stripped = decode_file(
        &dir.path().join("strip/rotated.png"),
        DecodeOptions::default(),
    )
    .unwrap();
    assert!(stripped.exif.is_none());
}

#[test]
fn overwriting_the_input_in_place_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("big.png");
    write_image(&input, Format::Png, 300, 300);
    let mut opts = ConvertOptions::new(Format::Png);
    opts.conflict = ConflictPolicy::Overwrite;
    opts.resize = Some(ResizeSpec::Percent(50.0));
    let jobs = plan(std::slice::from_ref(&input), &opts);
    assert_eq!(jobs[0].output, input);
    let report = run(jobs, &opts, |_| {});
    assert_eq!(report.converted(), 1);
    let out = decode_file(&input, DecodeOptions::default()).unwrap();
    assert_eq!(out.image.width(), 150);
}

#[test]
fn cli_convert_and_info() {
    let exe = env!("CARGO_BIN_EXE_chitra");
    let dir = tempfile::tempdir().unwrap();
    write_image(&dir.path().join("x.png"), Format::Png, 64, 32);
    write_image(&dir.path().join("y.bmp"), Format::Bmp, 64, 32);

    let out = Command::new(exe)
        .args([
            "convert",
            "--to",
            "jpg",
            "-q",
            "80",
            "--resize",
            "50%",
            "--dry-run",
        ])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!dir.path().join("x.jpg").exists());

    let out = Command::new(exe)
        .args(["convert", "--to", "jpg", "--resize", "50%", "--out"])
        .arg(dir.path().join("o"))
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("2 converted"));
    let d = decode_file(&dir.path().join("o/x.jpg"), DecodeOptions::default()).unwrap();
    assert_eq!((d.image.width(), d.image.height()), (32, 16));

    let out = Command::new(exe)
        .arg("info")
        .arg(dir.path().join("o/x.jpg"))
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("32 x 16"));

    let bad = Command::new(exe)
        .args(["convert", "--to", "tiff", "x"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
    let version = Command::new(exe).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("chitra "));
}

#[test]
fn console_cli_converts() {
    let exe = env!("CARGO_BIN_EXE_chitra-cli");
    let dir = tempfile::tempdir().unwrap();
    write_image(&dir.path().join("x.png"), Format::Png, 40, 20);

    let out = Command::new(exe)
        .args(["convert", "--to", "webp", "--out"])
        .arg(dir.path().join("o"))
        .arg(dir.path().join("x.png"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 converted"));
    let d = decode_file(&dir.path().join("o/x.webp"), DecodeOptions::default()).unwrap();
    assert_eq!((d.image.width(), d.image.height()), (40, 20));

    let no_command = Command::new(exe).output().unwrap();
    assert_eq!(no_command.status.code(), Some(2));
}
