# Local patch

This is `zenjpeg` 0.7.1 from crates.io (AGPL-3.0-only or commercial, the same
terms as `jxl-encoder`), used through `[patch.crates-io]` in the top-level
`Cargo.toml`. `jxl-encoder` needs it to read JPEG files for lossless JPEG to
JPEG XL repacking. One change, in `src/encode/dct.rs`:

- `mf32x8::load_8x8` takes the CPU token as its first argument in the
  `magetypes` version Chitra is held at (0.9.26, see the CI workflow), so
  the two AVX2 call sites now pass it. Without this the crate does not
  compile against that `magetypes`.

Drop this directory and the `[patch.crates-io]` entry once `jxl-encoder`
depends on a `zenjpeg` release that builds with the locked `magetypes`.
