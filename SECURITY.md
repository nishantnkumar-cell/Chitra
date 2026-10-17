# Security policy

## Reporting a vulnerability

Please report security problems privately, not in a public issue. On GitHub,
open the repository's **Security** tab and choose **Report a vulnerability**.
Include the Chitra version (`chitra-cli --version`), your operating system, and
if possible the file that triggers the problem.

You should get a reply within a week. Fixes are released as a new version and
described in the release notes.

## Supported versions

Only the latest release gets security fixes.

## Security design

Chitra opens files that may come from anywhere, so it is built to treat every
photo as untrusted input.

**Memory-safe decoding.** Every decoder and encoder is written in Rust. The
HEIC (HEVC) and AVIF (AV1) decoders are pure Rust with no `unsafe` code, and
no system or C image codecs run inside Chitra. Chitra's own library code is
`#![forbid(unsafe_code)]`; the only `unsafe` in the app is one Windows API
call that attaches to the terminal for command-line output.

**Resource limits.** Decoders are capped at 2 GiB of memory, so a small file
that claims enormous dimensions (a "decompression bomb") is refused instead
of exhausting memory.

**Failure isolation.** If a decoder hits an internal error on a damaged
file, only that file fails with "cannot decode"; the viewer stays open and a
batch conversion carries on with the other files.

**Safe writes.** Conversions write to a temporary file and rename it into
place, so a failure never leaves a half-written output or damages an input
that is being overwritten.

**No network access.** Chitra makes no network connections: no telemetry, no
update checks, no online lookups, and it never starts other programs. CI
fails if a networking or TLS library is ever added. The Linux Flatpak runs
without network permission.

**File-system scope.** Folder scans do not follow symbolic links or
junctions, so a link loop cannot trap a scan. Deleting a photo moves it to
the Recycle Bin or Trash rather than erasing it.

**Hardened Windows executables.** Built with ASLR (high entropy), DEP and
Control Flow Guard; CI checks these flags on every build. The C runtime is
linked statically.

**Least-privilege installers.** The Windows installer installs per user
without admin rights and adds no services, scheduled tasks or startup
entries; uninstalling removes everything it added. The Linux tarball installs
into `~/.local` without root.

**Dependency checks.** `cargo deny` runs on every push and fails on any
RustSec security advisory, yanked crate, banned crate, or crate from outside
crates.io. The one modified dependency, `oxideav-avif`, is kept in
`vendor/` with its change described in `vendor/oxideav-avif/PATCHED.md`.

## Testing

The test suite feeds truncated and corrupted files to every decoder. Before
release, the decoders were also tested with 660 randomly truncated and
corrupted HEIC, AVIF, JPEG XL, WebP, JPEG, PNG, GIF and BMP files (no
crashes; each was reported as "cannot decode") and with a PNG claiming
60000×60000 pixels (refused in 0.2 seconds).

To check the dependencies yourself:

```
cargo install cargo-deny cargo-audit --locked
cargo deny check advisories bans sources
cargo audit
```
