# Chitra user guide

Chitra is a photo viewer and batch converter. It opens JPEG, JPEG XL, PNG, GIF,
BMP, WebP, AVIF and HEIC/HEIF (iPhone) photos, and converts between them. It
works offline and never connects to the internet.

- [Installing](#installing)
- [Opening photos](#opening-photos)
- [Moving between photos](#moving-between-photos)
- [Zoom, rotate and full screen](#zoom-rotate-and-full-screen)
- [Thumbnail grid and folder overview](#thumbnail-grid-and-folder-overview)
- [Slideshow](#slideshow)
- [Photo info](#photo-info)
- [Copy, delete and refresh](#copy-delete-and-refresh)
- [Converting photos](#converting-photos)
- [Settings](#settings)
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
| `F`, `F11` or double-click | Full screen on / off |
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
| `Delete` or **Delete** | Move the photo to the Recycle Bin / Trash (asks first) |
| `F5` | Re-read the folder |

## Converting photos

### In the app

Click **Convert** or press `Ctrl+S`. Choose:

- **Files:** this photo, or all photos in the folder.
- **Format:** JPEG, JPEG XL, PNG, GIF, BMP, WebP or AVIF. HEIC photos can be
  converted *from*, but not *to*.
- **Quality:** 1 to 100 (90 by default), or **Lossless** for WebP and JPEG XL.
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

## Settings

Click **Settings** or press `Ctrl+,`. Changes apply immediately.

| Section | Options |
|---|---|
| Slideshow | Seconds per photo |
| Mouse | What the wheel does: next/previous photo and zoom once zoomed in (default), always next/previous (`Ctrl`+wheel zooms), or always zoom. Whether right-click returns to fit. Whether wheel tilt and the Back/Forward buttons change photo |
| Sorting | Name (natural order: 2 before 10), date modified, file size or type; ascending or descending |
| Folders | Show the folder overview; thumbnail size |
| Theme | Same as Windows/system (default), Light or Dark |

**Restore defaults** resets everything. Settings are stored in
`%APPDATA%\Chitra\settings.ini` (Windows) or `~/.config/Chitra/settings.ini`
(Linux).

## Keyboard and mouse reference

Press `F1` or **?** in the app for this list.

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

**Converting to AVIF is slow.** AVIF compresses very well but takes several
seconds per large photo. Use WebP or JPEG XL when speed matters.

**Typing `chitra convert` in PowerShell returns immediately.** Use
`chitra-cli` instead; `chitra.exe` is the windowed app, so the terminal does
not wait for it.

**The wheel zooms instead of changing photo.** You are zoomed in.
Right-click to fit the photo, or choose "Always next / previous photo" in
Settings.
