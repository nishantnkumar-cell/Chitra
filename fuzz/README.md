# Fuzzing

Coverage-guided fuzzing of everything Chitra parses from outside:

| Target | What it feeds |
|---|---|
| `decode_jpeg`, `decode_png`, `decode_gif`, `decode_webp`, `decode_bmp`, `decode_jxl`, `decode_avif`, `decode_heif` | the decoder of that format, as a still and as an animation |
| `decode_auto` | the format the first bytes say |
| `pth` | PyTorch model weights (zip and pickle) |
| `release_json` | the JSON of the update check |
| `rename` | rename patterns |
| `exif` | EXIF blocks, including removing the location |

The targets call `chitra::fuzzing`, which runs the parsers **without** the panic
net the app puts around codecs, so a panic is reported as the bug it is.

```sh
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run decode_avif          # until stopped, or add: -- -max_total_time=60
```

GitHub Actions runs each target for a few minutes every week (`.github/workflows/fuzz.yml`)
and for 30 seconds when this folder changes; a crash fails the run and its input is
uploaded. A quicker, deterministic cousin runs with the normal tests:
`cargo test --test robustness`.
