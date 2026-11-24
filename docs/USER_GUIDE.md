# Chitra user guide

Chitra is a photo viewer and batch converter. It opens JPEG, JPEG XL, PNG, GIF,
BMP, WebP, AVIF and HEIC/HEIF (iPhone) photos, and converts between them. It
works offline. The only times it goes online are a daily check for a new
version (which you can turn off) and AI model downloads you ask for.

- [Installing](#installing)
- [Opening photos](#opening-photos)
- [Moving between photos](#moving-between-photos)
- [Zoom, rotate and full screen](#zoom-rotate-and-full-screen)
- [Thumbnail grid and folder overview](#thumbnail-grid-and-folder-overview)
- [Slideshow](#slideshow)
- [Photo info](#photo-info)
- [Copy, delete and refresh](#copy-delete-and-refresh)
- [Converting photos](#converting-photos)
- [Upscale and sharpen](#upscale-and-sharpen)
- [Settings](#settings)
- [Menu, About and updates](#menu-about-and-updates)
- [Keyboard and mouse reference](#keyboard-and-mouse-reference)
- [Troubleshooting](#troubleshooting)

## Installing

### Windows 10 and 11

1. Download `Chitra-Setup-<version>-x64.exe` and run it.
2. If Windows shows **Windows protected your PC**, click **More info**, then
   **Run anyway**. (The installer is not code-signed yet.)
3. Choose **Install for me only** (no admin rights needed) or **all users**.
4. Optionally tick **Create a desktop shortcut**.

Chitra then appears in the Start menu and under **Open with** for every
supported format. To make it your default photo viewer, right-click a photo,
choose **Open with > Choose another app**, pick **Chitra** and tick **Always**.

To remove it: **Settings > Apps > Installed apps > Chitra > Uninstall**.

### Linux (KDE Plasma and GNOME)

Flatpak (recommended):

```
flatpak install --user Chitra-<version>-x86_64.flatpak
```

On Ubuntu 24.04+ or Debian 13+ you can use the .deb package instead:

```
sudo apt install ./chitra_<version>_amd64.deb      # sudo apt remove chitra removes it
```

Or the tarball, which installs into your home folder without root:

```
tar xzf chitra-<version>-linux-x86_64.tar.gz
chitra-<version>-linux-x86_64/install.sh          # add --uninstall to remove
```

Chitra then appears in the app menu and under **Open With** in Dolphin and
Files.

## Opening photos

- Double-click a photo (if Chitra is the default), or right-click it and use
  **Open with > Chitra**.
- Drag a photo or folder onto the Chitra window.
- **Open** in the toolbar or `Ctrl+O` opens a photo; `Ctrl+Shift+O` opens a
  folder.
- On Windows, right-click a folder and choose **Browse with Chitra** (on
  Windows 11 it is under **Show more options**).

When you open one photo, Chitra lists every supported photo in the same folder
so you can move through them.

## Moving between photos

| Do this | To |
|---|---|
| Right arrow, Space, Page Down | Next photo (wraps to the first at the end) |
| Left arrow, Backspace, Page Up | Previous photo |
| Home / End | First / last photo |
| Mouse wheel down / up | Next / previous photo |
| Tilt the wheel right / left | Next / previous photo |
| Mouse Forward / Back side buttons | Next / previous photo |
| `‹` and `›` in the toolbar | Previous / next photo |

When you are zoomed in, the wheel zooms instead of changing photo, so you can
look closely without jumping away. **Right-click** to return to fit-to-window,
and the wheel changes photo again. You can change this in
[Settings](#settings).

## Zoom, rotate and full screen

| Do this | To |
|---|---|
| `Ctrl` + mouse wheel, `+` / `-` | Zoom in / out at the mouse pointer |
| `0` or **Fit** | Fit the photo to the window |
| `1` or **1:1** | Show actual pixels |
| Drag | Move around a zoomed photo |
| Right-click | Back to fit-to-window |
| `R` / `Shift+R` | Rotate right / left (view only; the file is not changed) |
| `F`, `F11`, `Ctrl+Cmd+F` (Mac) or double-click | Full screen on / off |
| `Esc` | Leave full screen |

Photos taken sideways are turned upright automatically from their EXIF data.

## Thumbnail grid and folder overview

- Press `G` or `Enter`, or click **Browse**, to switch between the single
  photo and a **thumbnail grid** of the folder. Click a thumbnail to open it.
  `Ctrl` + wheel or the slider in the toolbar changes the thumbnail size.
- When a folder has sub-folders, Chitra shows the **folder overview**: the
  photos directly in the folder in the first row, then one row per sub-folder
  with its name, how many photos it has, its first photos, and an
  **Open folder** tile at the end.
  - Click a photo to view it; the arrows and wheel then move through that
    photo's folder.
  - Click **Open folder** to browse that sub-folder.
  - **Up**, `Backspace` or the mouse Back button goes to the parent folder.
  - **Folders** in the toolbar switches between the overview and the plain
    grid. You can turn the overview off in Settings.

## Videos

Phone videos (`.mp4`, `.mov`, `.m4v`, `.3gp` from Android phones and iPhones)
are listed with the photos and have a play mark on their thumbnails. Open one
and it starts playing with sound. Move the mouse to show the control bar:

| Do this | To |
|---|---|
| `Space`, `K`, click the video, or the play button | Play / pause |
| Drag or click the position slider | Jump to that point |
| `Shift+Left` / `Shift+Right` | Back / forward 5 seconds |
| `M` or the speaker button | Mute / unmute |
| Drag the volume slider | Volume (remembered for next time) |
| `Right` / `Left` | Next / previous file (the video stops) |

During a slideshow, a video plays to its end before the next file. The info
panel (`I`) shows the video's length, size and codecs. Videos are decoded by
the system (Media Foundation on Windows, AVFoundation on macOS, GStreamer on
Linux), so they play with the same codecs as the system's own video app.

## Slideshow

Press `S` or click **Slideshow** to start; press `S` or `Esc` to stop.
The seconds box next to **Slideshow**, or the `[` and `]` keys, set how long
each photo stays (1 to 60 seconds, 3 by default).

## Photo info

Press `I` or click **Info** to show the file's format, size, dimensions and
EXIF details: camera, lens, exposure, date taken and GPS position. The status
bar at the bottom always shows the position in the folder, file name,
dimensions, format, file size and zoom.

## Copy, delete and refresh

| Do this | To |
|---|---|
| `Ctrl+C` | Copy the image to the clipboard |
| `Ctrl+Shift+C` | Copy the file's path |
| `Delete`, `Cmd+Backspace` (Mac) or **Delete** | Move the photo to the Recycle Bin / Trash (asks first) |
| `F5` | Re-read the folder |

## Printing

Press `Ctrl+P` (`Cmd+P` on a Mac) or click **Print** in the toolbar to print
the photo you are looking at. It prints the way it is shown on screen
(including any rotation you applied with `R`), as large as fits on the paper
with a small border, and turned to suit the paper.

- On **Windows**, the usual Windows Print dialog opens: pick the printer,
  paper size and number of copies there.
- On **Linux** (KDE Plasma or GNOME), your desktop's print dialog opens.
- On a **Mac**, and on Linux desktops without a print dialog service,
  Chitra's own Print window opens: pick the printer, paper, **Fit to page**
  (the whole photo) or **Fill page** (edge to edge, trimming the sides) and
  copies. **Save as PDF…** saves the page instead of printing it.

In the **Passport photo** window, **Print…** prints the print sheet or the
single photo at its exact size, with no scaling, so each photo comes out at
the official size. Put paper of the sheet's size (for example 6 x 4 in) in
the printer, or a larger sheet and cut along the guides.

## Converting photos

### In the app

Click **Convert** or press `Ctrl+S`. Choose:

- **Files:** this photo, or all photos in the folder.
- **Format:** JPEG, JPEG XL, PNG, GIF, BMP, WebP or AVIF. HEIC photos can be
  converted *from*, but not *to*.
- **Quality:** 1 to 100 (90 by default), or **Lossless** for WebP and JPEG XL.
- **JPEG files** (JPEG XL only): **Repack losslessly** is on by default. A
  JPEG converted to JPEG XL keeps its own compressed data, so the new file is
  about 20% smaller with exactly the same picture, and quality settings don't
  apply. Photos you resize, or that are turned by their EXIF orientation, are
  re-encoded instead. Turn it off to re-encode every JPEG at the chosen quality.
- **Resize** (optional): limit the longest side. Small photos are never
  enlarged.
- **Save to:** the same folder as each photo (default) or a folder you choose.
- **File name:** `{name}` keeps the original name; tokens `{parent}`, `{n}`
  and `{n:03}` add the folder name or a running number.
- **If file exists:** add a number (default), skip, or overwrite.
- **Metadata:** keep EXIF and colour profile (on by default).

For sub-folders, percentage or box resizing, use the
[command line](#from-the-command-line). Converted photos always display the
right way up. A failed file never leaves
a half-written output behind, and the summary lists any failures.

Which format to pick:

| Format | Good for |
|---|---|
| JPEG | Sharing anywhere; every device opens it |
| WebP | Websites; much smaller than JPEG |
| AVIF | Smallest files at good quality; slow to create |
| JPEG XL | Archiving; small files, lossless option, fast |
| PNG | Screenshots and graphics; lossless |
| GIF | Simple animations |
| BMP | Old software that needs it |

### From the command line

Use `chitra-cli` (Windows: `chitra-cli.exe` in the install folder). Examples:

```
chitra-cli convert photos\ --to webp --out web\ --resize 2048
chitra-cli convert *.png --to jpg -q 85 --rename "holiday_{n:03}"
chitra-cli convert scans\ -r --to jxl --out archive\
chitra-cli convert iphone\*.heic --to jpg
chitra-cli info photo.jpg
```

Run `chitra-cli convert --help` for every option. Exit codes: `0` all
converted, `1` some files failed or none were found, `2` invalid options.

## Upscale and sharpen

**Enhance** in the toolbar (outlined, always available; or `Ctrl+E`, or
**Menu > Enhance and upscale**) enlarges the current photo 1x, 2x, 3x
or 4x and sharpens it, for old or small photos such as 960 x 1280 phone
pictures. The window shows the original and the result side by side; drag
either one to move both, use the wheel to zoom, and double-click to switch
between fit and 100%. Only the part on screen is worked out, so the preview
keeps up while you move the sliders.

- **Size**: 1x (sharpen only) to 4x. Enlarging uses Lanczos3, the sharpest of
  the usual resampling filters.
- **Reduce noise**: 0 to 100 (30 by default). Smooths grain and the colour
  blotches of old JPEGs before enlarging, keeping edges sharp, so the
  enlargement and the sharpening don't make the noise worse. Too high looks
  waxy.
- **Sharpen**: 0 to 300%; 80% is a good start. The sharpening works on
  brightness only, so colours do not fringe.
- **Radius**: the size of the detail to bring out. **Auto** picks one for the
  size (1.0 px at 1x up to 2.2 px at 4x).
- **Ignore noise below**: small changes in brightness (grain, JPEG blocks) are
  sharpened less. Raise it for noisy or heavily compressed photos.

**Save** writes this photo or every photo in the folder as JPEG, JPEG XL or
PNG, named `{name}_2x` (and so on) by default, next to the originals or in a
folder you choose. The Classic method does not invent detail; it makes
enlargements smooth and crisp, and stays fast and small.

### AI method (optional)

**Method: AI** uses Real-ESRGAN models to rebuild detail and remove noise and
JPEG artefacts while enlarging. They run inside Chitra in plain Rust, on the
graphics card when there is one (Vulkan on Windows and Linux, DirectX 12 on
Windows, Metal on macOS) and otherwise on the processor, with no Python, CUDA
or ONNX runtime needed. Pick one under **AI model**:

| Model | Download | Good for |
|---|---|---|
| **Fast** | 10 MB | Quick general model; removes noise and JPEG artefacts |
| **Best** | 67 MB | Most detail, especially faces and small or old photos |
| **Best 2x** | 67 MB | The Best model trained for 2x; four times quicker than Best for 2x results |
| **Faithful** | 67 MB | Invents less texture than Best (fabric, skin, plastic); softer |
| **Drawings** | 18 MB | Drawings, cartoons and anime: clean lines and flat colour |

No model is part of the Chitra download. The first time you pick one,
**Download** fetches it from the Real-ESRGAN releases on GitHub into the
Chitra settings folder (`%APPDATA%\Chitra\models`,
`~/Library/Application Support/Chitra/models` or `~/.config/Chitra/models`)
and checks its SHA-256. Without internet access (the Flatpak has none),
download the file yourself (`chitra-cli ai-model list` prints the links) and
use **Choose files…**.

- **AI noise removal** (Fast only): 0 keeps more fine texture, 100 removes
  the most noise. It blends the Fast model's two files.
- **Sharpen** starts at 0% for AI, since the result is already sharp.

Each model works at its own size (4x, or 2x for Best 2x) and the result is
then resized to the size you chose. On the processor the AI is much slower
than Classic: Fast takes about half a minute for a 960 x 1280 photo at 4x on
a 4-core processor, and the Best, Faithful and Best 2x models take around ten
times longer, so use them on a graphics card or for small photos. The
preview shows a smaller area for AI, and the window says whether the graphics
card or the processor was used. Files are named `{name}_2x_ai` and so on.

From the command line, `--scale` and `--sharpen` work with any output format,
and `--ai` switches to the AI method (at the model's own size unless
`--scale` says otherwise):

```
chitra-cli convert old\ --to jpg --scale 2 --denoise 30 --sharpen 80 --rename "{name}_2x"
chitra-cli ai-model download fast best
chitra-cli convert old\ --to png --ai --ai-noise 50 --rename "{name}_4x_ai"
chitra-cli convert old\ --to png --ai-model best --scale 2 --rename "{name}_2x_best"
```

`chitra-cli ai-model list` shows each model and whether it is installed,
`chitra-cli ai-model download all` gets every model, and
`chitra-cli ai-model import FILE...` installs files you downloaded yourself.
In the Enhance window, **Run the AI on** says whether the AI will use a
graphics card or the processor, and after each preview shows what it really
used. Leave it on **Automatic** (the fastest card, a discrete one before an
integrated one) or pick a card yourself, or **Processor only**, if
previews are slow or Automatic chose the wrong card. **Look again** searches
for cards again after a driver install. Your choice is remembered.
Setting `CHITRA_AI_CPU=1` still keeps the AI off the graphics card.

## Settings

Choose **Menu > Settings** or press `Ctrl+,`. Changes apply immediately.

| Section | Options |
|---|---|
| Slideshow | Seconds per photo |
| Mouse | What the wheel does: next/previous photo and zoom once zoomed in (default), always next/previous (`Ctrl`+wheel zooms), or always zoom. Whether right-click returns to fit. Whether wheel tilt and the Back/Forward buttons change photo |
| Sorting | Name (natural order: 2 before 10), date modified, file size or type; ascending or descending |
| Folders | Show the folder overview; thumbnail size |
| Theme | Same as Windows/system (default), Light or Dark |
| Updates | Check for new versions daily (asked at first start); **Check now** |

**Restore defaults** resets everything. Settings are stored in
`%APPDATA%\Chitra\settings.ini` (Windows) or `~/.config/Chitra/settings.ini`
(Linux).

## Menu, About and updates

**Menu** at the end of the toolbar lists everything Chitra does, with the
keyboard shortcuts: open, convert, enhance, passport photo, print, settings,
full screen, keyboard shortcuts, the user guide, problem reports, update
checks and **About Chitra**.

**About Chitra** shows the version, the licence (AGPL-3.0-or-later), links to
the website, user guide and issue tracker, the third-party software Chitra is
built on with their licences, and system information you can copy into a
problem report.

**Updates.** The first time Chitra starts it asks whether to check for
updates; nothing goes online until you answer. If you agree, once a day Chitra asks GitHub for the latest release (only the
public release list is read; nothing about you or your photos is sent). When a
newer version is out, an **Update** button appears in the toolbar:

- Windows: **Download and install** fetches the installer, checks its SHA-256
  against the one GitHub publishes, closes Chitra and runs it. Settings and
  AI models are kept.
- Linux tarball: **Download** saves the new tarball to your Downloads folder
  (checksum checked); extract it and run `install.sh`.
- Flatpak and macOS: **Open download page**. The Flatpak has no network
  access, so it cannot check by itself; use **Menu > Check for updates** to
  open the releases page.

Turn the daily check off in Settings. **Menu > Check for updates** checks at
any time.

## Keyboard and mouse reference

Press `F1` or **Menu > Keyboard shortcuts** in the app for this list.

| Key or mouse | Action |
|---|---|
| Right, Space, PgDn / Left, Backspace, PgUp | Next / previous photo |
| Home / End | First / last photo |
| Wheel, wheel tilt, Back/Forward buttons | Previous / next photo |
| Right-click | Fit to window |
| Ctrl+wheel, + / - | Zoom |
| 0 / 1 | Fit / actual pixels |
| R / Shift+R | Rotate right / left |
| F, F11, double-click | Full screen |
| G or Enter | Photo / thumbnail grid |
| I | Info panel |
| S | Slideshow |
| [ and ] | Slideshow slower / faster |
| Delete | Move to Recycle Bin / Trash |
| Ctrl+C / Ctrl+Shift+C | Copy image / copy path |
| Ctrl+O / Ctrl+Shift+O | Open file / folder |
| Ctrl+S | Convert |
| Ctrl+P (Cmd+P on Mac) | Print |
| Ctrl+E | Upscale and sharpen |
| Ctrl+Shift+P | Passport photo |
| Ctrl+, | Settings |
| F5 | Refresh folder |
| F1 | Shortcut list |
| Esc | Leave full screen, close a dialog, or quit |

## Troubleshooting

**Windows says "Windows protected your PC".** The installer is not
code-signed. Click **More info**, then **Run anyway**.

**A photo shows "cannot decode".** The file is damaged or not really in the
format its extension says. Chitra skips it and carries on with the rest of the
folder or batch.

**A video says "Cannot play this video".** The system has no decoder for it.
On Windows, iPhone videos (HEVC) need **HEVC Video Extensions** from the
Microsoft Store. On Linux, install `gstreamer1.0-libav` (Ubuntu, Debian) or
`gstreamer1-libav` (Fedora, from RPM Fusion).

**Converting to AVIF is slow.** AVIF compresses very well but takes several
seconds per large photo. Use WebP or JPEG XL when speed matters.

**Typing `chitra convert` in PowerShell returns immediately.** Use
`chitra-cli` instead; `chitra.exe` is the windowed app, so the terminal does
not wait for it.

**The wheel zooms instead of changing photo.** You are zoomed in.
Right-click to fit the photo, or choose "Always next / previous photo" in
Settings.
