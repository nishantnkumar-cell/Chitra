# Chitra

A fast, small, memory-safe photo viewer and batch converter for Windows and
Linux (KDE Plasma and GNOME); a macOS (Apple Silicon) version is in progress.
Use the installer, or run the portable `.exe` without installing anything.

**Documentation:** [User guide](docs/USER_GUIDE.md) ·
[Building and packaging](docs/BUILDING.md) · [Security policy](SECURITY.md)

Formats: **JPEG, JPEG XL, PNG, GIF, BMP, WebP, AVIF**, including animated GIF,
WebP, APNG and JPEG XL. **HEIC/HEIF** photos (as taken by iPhones) open too,
and can be converted to any of the other formats; Chitra does not write HEIF.

## Install

### Windows

Run `Chitra-Setup-<version>-x64.exe` (Windows 10 or 11, 64-bit). It installs
for your account without needing admin rights; the first page also offers an
install for all users. You get:

- Chitra in the Start menu, and a desktop icon if you tick it;
- Chitra under **Open with** for JPEG, JPEG XL, PNG, GIF, BMP, WebP, AVIF and HEIC, and
  **Browse with Chitra** on folders (on Windows 11 it is under "Show more
  options"). Windows only lets you change your default photo app yourself:
  Open with, then Choose another app, then Always;
- `chitra-cli.exe` next to `chitra.exe` for scripted conversion;
- an uninstaller in Settings, then Apps.

The installer is not code-signed yet, so Windows SmartScreen may say it is
from an unknown publisher: click **More info**, then **Run anyway**.

CI builds the installer on every push (the `chitra-windows-installer`
artifact) from `installer/chitra.iss` with [Inno Setup](https://jrsoftware.org/isinfo.php).
Pushing a tag such as `v0.1.0` drafts a GitHub release with the installer and
both exes attached; it stays a draft until someone publishes it.

### Linux (KDE Plasma and GNOME)

**Flatpak** (any distribution with Flatpak, such as Fedora, openSUSE, Kubuntu
or KDE neon; on Ubuntu run `sudo apt install flatpak` first):

```
flatpak install --user Chitra-<version>-x86_64.flatpak
```

**Tarball** (no Flatpak needed; installs into `~/.local`, no root):

```
tar xzf chitra-<version>-linux-x86_64.tar.gz
chitra-<version>-linux-x86_64/install.sh        # --uninstall removes it
```

Either way Chitra appears in the app menu and under **Open With** in Dolphin and
Files for every format it reads. On both desktops it:

- runs natively on Wayland (X11 still works);
- follows the desktop's light or dark style live when the theme is "Same as
  system";
- uses the desktop's own Open and Save dialogs (through the desktop portal);
- moves deleted photos to the desktop's Trash;
- on GNOME, draws a GNOME-style title bar (drag to move, double-click to
  maximize); on KDE Plasma, KWin draws its usual one.

The Flatpak has no network access. It can read and write your home folder and
external drives (`/media`, `/run/media`, `/mnt`) so the folder browser and batch
converter work anywhere you keep photos. Builds target x86-64-v2 CPUs (Intel
since 2008, AMD since 2011) and use faster AVX2 code at run time where the CPU
has it. CI builds both packages on every push (the `chitra-linux-x86_64`
artifact) and checks that they install and run.

## Viewer

Open a photo by double-clicking it (set Chitra as the default app or use
"Open with"), by dragging it onto the window, or with `Ctrl+O`.
Chitra lists the rest of the folder and you move through it with the keyboard.

| Key | Action |
|---|---|
| Right, Space, PgDn | Next photo (wraps around) |
| Left, Backspace, PgUp | Previous photo |
| Home / End | First / last photo |
| G or Enter | Switch between photo and thumbnail grid |
| Mouse wheel, wheel tilt, mouse Back/Forward | Previous / next photo (when zoomed in, the wheel zooms; changeable in Settings) |
| Right-click | Back to fit-to-window, so the wheel changes photo again |
| Ctrl+wheel, + / - | Zoom (in the grid: thumbnail size) |
| 0 / 1 | Fit to window / actual pixels |
| Drag | Pan when zoomed in |
| R / Shift+R | Rotate view right / left |
| F, F11, Ctrl+Cmd+F (Mac), double-click | Full screen |
| I | Info panel with EXIF (camera, lens, exposure, GPS) |
| S | Slideshow (3 s per photo by default) |
| [ and ] | Fewer or more seconds per photo (1 to 60), also in Settings |
| Delete, Cmd+Backspace (Mac) | Move to Recycle Bin (asks first) |
| Ctrl+C / Ctrl+Shift+C | Copy image / copy file path |
| Ctrl+O / Ctrl+Shift+O | Open file / open folder |
| Ctrl+S | Convert dialog |
| Ctrl+P | Passport photo |
| Ctrl+, | Settings |
| F5 | Refresh folder |
| F1 | Shortcut list |
| Esc | Leave full screen, close a dialog, or quit |

**Settings** (toolbar button or `Ctrl+,`) holds the slideshow timer, what the
mouse wheel does (next/previous photo, zoom, or both), whether right-click
returns to fit and whether wheel tilt and Back/Forward change photo, sorting
(name in natural order, date modified, size, type; ascending or descending),
the folder overview and thumbnail size, and the theme: **Same as Windows**
(the default), **Light** or **Dark**. Changes apply at once and are saved in
`%APPDATA%\Chitra\settings.ini` on Windows and `~/.config/Chitra/settings.ini`
on Linux.

**Folder overview:** open a folder that has sub-folders and Chitra shows one
row per sub-folder with its first photos (as many as fit the window), then an
**Open folder** tile that browses it. Photos directly in the folder get the
first row. Click a photo to view it; the arrow keys and wheel then move
through that photo's folder. **Up** (or Backspace, or the mouse Back button,
in the overview) goes to the parent folder, and **Folders** in the toolbar
switches between the overview and the plain thumbnail grid. Folders with no
photos and no sub-folders are left out.
EXIF orientation is applied automatically.

**Why it feels fast:** the window opens before anything is decoded; the
current photo is decoded first, then the next two and the previous one in the
background, so arrow keys usually show an image that is already on the GPU.
Photos are decoded at screen size and only reloaded at full resolution when
you zoom in. Holding an arrow key cancels decodes you have already skipped.

## Batch converter

From the viewer: **Convert** (or `Ctrl+S`) converts the current photo or the
whole folder. From a terminal or script, use `chitra-cli.exe`, the console
build of the same commands (cmd and PowerShell wait for it and get its exit
code; `chitra.exe` is a windowed app, so they return to the prompt at once):

```
chitra-cli convert photos\ --to webp --out web\ --resize 2048
chitra-cli convert *.png --to jpg -q 85 --rename "holiday_{n:03}"
chitra-cli convert scans\ -r --to jxl --out archive\
chitra-cli convert iphone\*.heic --to jpg
chitra-cli info photo.jpg
```

Exit codes: 0 all converted, 1 some files failed (or none were found), 2 bad arguments.

| Option | Meaning |
|---|---|
| `-t, --to FORMAT` | jpg, jxl, png, gif, bmp, webp, avif (HEIC can be read, not written) |
| `-q, --quality N` | 1-100 for JPEG, WebP, JPEG XL and AVIF (default 90); palette quality for GIF |
| `--lossless` | Lossless WebP / JPEG XL (see below) |
| `-s, --resize SIZE` | `2048` longest side, `50%`, or `1920x1080` box; never enlarges unless `--upscale` |
| `-o, --out DIR` | Output folder (default: next to each input) |
| `-n, --rename PATTERN` | Tokens `{name}` `{parent}` `{n}` `{n:03}` `{ext}` |
| `-r, --recursive` | Include sub-folders; the structure is mirrored under `--out` |
| `--on-conflict MODE` | `rename` (default, adds `_1`), `skip`, `overwrite` |
| `--strip-metadata` | Don't copy EXIF and colour profile |
| `--no-animation` | Only the first frame of animations |
| `-j, --jobs N` | Parallel workers (default: all cores) |
| `--dry-run` | Show what would be written |
| `--quiet` | Only print the summary and errors |

AVIF encoding is much slower than the other formats (several seconds for a
12-megapixel photo); batches still use every core.

Conversions run in parallel, write through a temporary file (so a failure
never leaves a half-written file or damages an input being overwritten), and
end with a report listing any failures. EXIF orientation is applied to the
pixels and reset in the copied EXIF, so outputs always display upright.

## Passport photos

**Passport** in the toolbar (or `Ctrl+P`) turns the current photo into a
passport or visa photo. Click the top of the head (the crown, not the top of
the hair), then the bottom of the chin; drag either marker to adjust, or
right-click to start again. Chitra scales the head to the middle of the
allowed size, straightens a tilted head, centres it, and shows a live preview
with the head guides drawn on it. Then save either:

- a **print sheet** at 300 dpi with grey cutting guides on 6 x 4 in,
  15 x 10 cm, 7 x 5 in, A4 or US Letter paper (for example 8 UK photos on a
  6 x 4 in print), or
- **one photo** as a JPEG for online applications, at the official pixel size
  where there is one (India 630 x 810, US 1200 x 1200) and 600 dpi otherwise.

Files are JPEGs tagged with their print resolution, so printing at actual
size gives the right physical size, and carry no EXIF (no location or camera
data). A slider moves the head size within the allowed range, and any area
the photo does not cover is filled with a colour you pick. For upload forms
with a size limit, **At most N KB** lowers the JPEG quality until the file
fits (on by default at 250 KB for India). **Custom size** takes any photo and
head size in mm.

| ID | Photo | Size | Head (chin to crown) |
|---|---|---|---|
| `uk` | United Kingdom passport | 35 x 45 mm | 29-34 mm |
| `eu` | EU passport / Schengen visa | 35 x 45 mm | 32-36 mm |
| `us` | United States passport / visa | 2 x 2 in | 1-1 3/8 in |
| `india` | India passport (2025 rules) | 35 x 45 mm | 36-38 mm |
| `india-visa` | India visa / OCI | 2 x 2 in | 1-1 3/8 in |
| `canada` | Canada passport | 50 x 70 mm | 31-36 mm |
| `australia` | Australia passport | 35 x 45 mm | 32-36 mm |
| `china` | China passport / visa | 33 x 48 mm | 28-33 mm |
| `japan` | Japan passport | 35 x 45 mm | 32-36 mm |

Rules change, so check the current ones of the office you apply to. The same
works from a terminal, given the two points in pixels:

```
chitra-cli passport me.jpg --country uk --crown 1520,610 --chin 1530,1640 --sheet 6x4
chitra-cli passport me.jpg --country india --crown 1520,610 --chin 1530,1640 -o upload.jpg
chitra-cli passport me.jpg --country custom --size 40x50 --head 30-34 --crown 1520,610 --chin 1530,1640 --sheet a4
chitra-cli passport --list
```

## Builds and licences

Chitra is free software under the [GNU Affero General Public License v3.0 or
later](LICENSE). You may use, share and modify it, including commercially;
if you distribute a modified version, or let people use one over a network,
you must publish its source under the same licence.

The lossy WebP and JPEG XL encoders (`zenwebp`, `jxl-encoder`) are
AGPL-3.0. `cargo build --release --no-default-features` leaves them out, and
then writes WebP and JPEG XL lossless only; viewing and every other format
are identical in both builds. The lossy encoders do not build on Apple Silicon
yet, so macOS ARM builds need `--no-default-features`.

## Memory safety

Every decoder and encoder is pure Rust, so the code that parses untrusted
files is memory safe:

| Part | Crate |
|---|---|
| JPEG, PNG, GIF, BMP, WebP | `image` (`zune-jpeg`, `png`, `gif`, `image-webp`) |
| JPEG XL decode | `jxl` (Google's jxl-rs, the decoder in Chrome and Firefox), multithreaded |
| AVIF decode | `oxideav-avif` (its own AV1 decoder, no `unsafe`; vendored with a fix for odd widths) |
| AVIF encode | `ravif` / `rav1e` (via `image`) |
| HEIF/HEIC decode | `heifer` (its own HEVC decoder, no `unsafe`; output matches libheif) |
| JPEG XL lossless encode | `zune-jpegxl` |
| Lossy WebP / JPEG XL encode | `zenwebp`, `jxl-encoder` |
| UI | `eframe`/`egui` (OpenGL) |

Chitra's own library code is `#![forbid(unsafe_code)]`. The only `unsafe` in
the app is one call to the Windows `AttachConsole` API so `chitra convert` can
print to the terminal it was started from. Decoders run with memory limits,
and tests feed truncated and corrupted files to every decoder.

## Security

- **Offline only.** Chitra makes no network connections: no telemetry, no
  update checks, and it never launches other programs. CI fails if a
  networking or TLS crate is ever added.
- **Untrusted files.** Decoders are capped at 2 GiB, so a small file claiming
  huge dimensions is refused. A bug inside a codec fails that one file
  ("cannot decode") instead of closing the app or stopping a batch.
- **Dependencies.** CI runs `cargo deny` (see `deny.toml`) on every push. It
  fails on any RustSec advisory, a yanked crate or a crate from outside
  crates.io. The one patched crate, `oxideav-avif`, lives in `vendor/` and
  `vendor/oxideav-avif/PATCHED.md` describes the change.
- **Windows exe.** Built with ASLR, DEP and Control Flow Guard, which CI checks.
  The installer is per-user and needs no admin rights.

## Building

```
cargo build --release                         # target\release\chitra.exe and chitra-cli.exe
cargo build --release --no-default-features   # without the lossy WebP/JXL encoders
cargo test
```

Rust 1.85 or newer. On Windows the C runtime is linked statically, so the exe
runs without the Visual C++ redistributable. CI builds and tests on Windows,
Linux and macOS and uploads the Windows exes as a build artifact.

Windows installer, from the repository folder in PowerShell (Inno Setup from
`winget install JRSoftware.InnoSetup`):

```
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
          "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
cargo build --release
& $iscc /Qp "/DAppVersion=$version" installer\chitra.iss   # dist\Chitra-Setup-<version>-x64.exe
```

Linux packages (tarball and Flatpak), including building them on Windows
through WSL, are covered step by step in [docs/BUILDING.md](docs/BUILDING.md).
