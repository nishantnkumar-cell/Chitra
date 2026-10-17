# Local patch

This is `oxideav-avif` 0.0.15 from crates.io (MIT, see LICENSE), used through
`[patch.crates-io]` in the top-level `Cargo.toml`. One change, in
`src/decoder.rs` (`infer_av1_pixmap`):

- 4:2:0 / 4:2:2 images with an odd width were rejected with "cannot infer AV1
  frame format", because the chroma stride is half the luma width rounded up,
  not exactly half. The check now accepts the rounded-up stride.

Drop this directory and the `[patch.crates-io]` entry once a release with the
fix is published.
