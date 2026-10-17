# Building and packaging Chitra

This covers building from source, running the tests, and making the release
packages: the Windows installer, the Linux tarball and the Flatpak. Everything
here runs on your own machine and costs nothing; GitHub Actions is optional.

- [Requirements](#requirements)
- [Build and test](#build-and-test)
- [Windows installer](#windows-installer)
- [Linux packages](#linux-packages)
- [Linux packages on Windows (WSL)](#linux-packages-on-windows-wsl)
- [Publishing a release](#publishing-a-release)
- [Continuous integration](#continuous-integration)
- [Security checks](#security-checks)

## Requirements

| | Windows 10/11 | Linux |
|---|---|---|
| Rust 1.85+ | `winget install Rustlang.Rustup` | `curl https://sh.rustup.rs -sSf \| sh` |
| C toolchain | Visual Studio Build Tools with "Desktop development with C++" | `build-essential pkg-config libxkbcommon-dev libwayland-dev` (Debian/Ubuntu) |
| Installer tool | Inno Setup 6: `winget install JRSoftware.InnoSetup` | `flatpak flatpak-builder desktop-file-utils appstream` for the Flatpak |
| Git | `winget install Git.Git` | your distribution's `git` |

Check the Rust toolchain with `rustc --version` and `cargo --version`. On
Windows, if a new terminal cannot find `cargo`, run
`$env:PATH += ";$env:USERPROFILE\.cargo\bin"`.

## Build and test

```
git clone https://github.com/nishantnkumar-cell/Chitra
cd Chitra
cargo test --release
cargo build --release
```

Outputs in `target/release/`:

| File | What |
|---|---|
| `chitra.exe` / `chitra` | The viewer (also runs `convert` and `info`) |
| `chitra-cli.exe` / `chitra-cli` | Console build for scripts; the terminal waits for it and gets its exit code |

The first build takes 5 to 15 minutes; later builds only recompile what
changed.

Feature flags:

| Command | WebP / JPEG XL output |
|---|---|
| `cargo build --release` | Lossy and lossless |
| `cargo build --release --no-default-features` | Lossless only (leaves out the `zenwebp` and `jxl-encoder` crates) |

Chitra is licensed AGPL-3.0-or-later either way.

## Windows installer

In PowerShell, from the repository folder:

```
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
          "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
cargo build --release
& $iscc /Qp "/DAppVersion=$version" installer\chitra.iss
```

Result: `dist\Chitra-Setup-<version>-x64.exe`.

`Select-String` needs `-Path` and `-Pattern` named, otherwise it treats the
pattern as a file name. A per-user `winget` install puts Inno Setup under
`%LOCALAPPDATA%\Programs`, a machine-wide one under `Program Files (x86)`;
the snippet checks both.

Test the installer without clicking through it:

```
.\dist\Chitra-Setup-$version-x64.exe /VERYSILENT /CURRENTUSER /NORESTART
& "$env:LOCALAPPDATA\Programs\Chitra\chitra-cli.exe" --version
& "$env:LOCALAPPDATA\Programs\Chitra\unins000.exe" /VERYSILENT /NORESTART
```

## Linux packages

On Linux (x86_64), from the repository folder:

```
cargo test --release
cargo build --release
sh tools/package_linux.sh
```

This writes `dist/chitra-<version>-linux-x86_64.tar.gz` and the unpacked files
in `dist/linux/`, which the Flatpak uses. Then the Flatpak:

```
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --disable-rofiles-fuse --force-clean \
    --repo=flatpak-repo flatpak-build packaging/flatpak/io.github.nishantnkumar_cell.Chitra.yml
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
    flatpak-repo dist/Chitra-<version>-x86_64.flatpak io.github.nishantnkumar_cell.Chitra
```

The first Flatpak build downloads the Freedesktop runtime and SDK (about
1 GB).

## Linux packages on Windows (WSL)

You can build the Linux packages on a Windows PC with WSL 2 (`wsl --install`
gives Ubuntu). Build inside the Linux file system, not under `/mnt/c`: it is
much faster and does not mix with the Windows `target` folder.

1. Open Ubuntu from the repository folder:

   ```
   cd C:\DevOps\Chitra
   wsl -d Ubuntu
   ```

2. Install the build tools (asks for your Ubuntu password once):

   ```
   sudo apt update
   sudo apt install -y build-essential pkg-config git curl libxkbcommon-dev libwayland-dev flatpak flatpak-builder desktop-file-utils appstream
   ```

3. Install Rust:

   ```
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   source ~/.cargo/env
   ```

4. Copy the code into Linux, then test, build and package it:

   ```
   git clone /mnt/c/DevOps/Chitra ~/chitra
   cd ~/chitra
   cargo test --release
   cargo build --release
   sh tools/package_linux.sh
   ```

5. Build the Flatpak with the commands in [Linux packages](#linux-packages).

6. Copy the results back to Windows:

   ```
   cp dist/*.tar.gz dist/*.flatpak /mnt/c/DevOps/Chitra/dist/
   exit
   ```

To pick up later changes, run `cd ~/chitra && git pull` and repeat from
step 4. WSL can also show the Linux viewer on the Windows desktop
(`./target/release/chitra`), which is handy for a quick check, but it is not a
real KDE or GNOME session.

## Publishing a release

Releases can be made entirely from your own machine with the GitHub CLI
(`winget install GitHub.cli`, then `gh auth login`). Uploading release files
is free on every GitHub plan.

1. Update `version` in `Cargo.toml`, commit and push.
2. Build the Windows installer and the Linux packages as above, so `dist\`
   holds:
   - `Chitra-Setup-<version>-x64.exe`
   - `chitra-<version>-linux-x86_64.tar.gz`
   - `Chitra-<version>-x86_64.flatpak`
3. Tag and publish:

   ```
   git tag v<version>
   git push origin v<version>
   gh release create v<version> --title "Chitra <version>" --generate-notes `
       dist\Chitra-Setup-<version>-x64.exe `
       dist\chitra-<version>-linux-x86_64.tar.gz `
       dist\Chitra-<version>-x86_64.flatpak
   ```

   Add `--draft` to check the release page before it goes public.

## Continuous integration

`.github/workflows/ci.yml` runs on GitHub Actions:

- tests on Windows, Linux and macOS;
- `cargo fmt`, `clippy` and `cargo deny`;
- the Windows exe size budget and hardening checks;
- the Windows installer and Linux packages, which are installed and run;
- a draft release when a `v*` tag is pushed.

GitHub Actions is free and unlimited for **public** repositories. Private
repositories get a monthly allowance in which macOS minutes count about ten
times and Windows minutes about twice; when it runs out, jobs stop with
"recent account payments have failed or your spending limit needs to be
increased". Keep the spending limit at $0 so nothing is ever charged, and
either make the repository public or build releases locally as described
above.

## Security checks

```
cargo install cargo-deny cargo-audit --locked
cargo deny check advisories bans sources   # same checks as CI (deny.toml)
cargo audit                 # RustSec vulnerability database
```

Both should report no vulnerabilities. `deny.toml` lists the one accepted
notice (`paste`, unmaintained, used only at compile time) and its reason.
See [SECURITY.md](../SECURITY.md) for the security design and how to report
a problem.
