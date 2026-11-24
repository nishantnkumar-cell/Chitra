# Code signing

The Windows installer and `.exe` files are **not signed yet**, so Windows
SmartScreen may show "unknown publisher". This page records the plan and what is
still to do.

## Plan: SignPath Foundation (free for open source)

[SignPath Foundation](https://signpath.org/) signs open-source releases at no
cost. Chitra fits their conditions: public repository, AGPL-3.0-or-later (an
OSI-approved licence), no proprietary parts, builds made by GitHub Actions.

### To do (maintainer, not started)

- [ ] Apply at <https://signpath.org/apply> using the text below.
- [ ] After approval: install the SignPath GitHub App on the repository, create
      the project and signing policy in SignPath, and add the repository secret
      `SIGNPATH_API_TOKEN` and variables for the organisation and project ids.
- [ ] Ask for the CI step to be added (see "CI changes" below).
- [ ] Add the attribution line to the README (see below) once signing is live.

### Application text

> **Project:** Chitra, a small, fast photo viewer and batch converter for
> Windows and Linux (JPEG, JPEG XL, PNG, GIF, BMP, WebP, AVIF, HEIC; AI
> upscaling; videos).
> **Repository:** https://github.com/nishantnkumar-cell/Chitra
> **Licence:** AGPL-3.0-or-later (OSI approved). No proprietary components.
> **Build:** GitHub Actions (`.github/workflows/ci.yml`) builds `chitra.exe`,
> `chitra-cli.exe` and the Inno Setup installer from a tagged commit, with
> `cargo deny`, tests, a Windows smoke test and a fuzzing run on every change.
> **Why signing is needed:** the installer is downloaded by end users on
> Windows; without a signature SmartScreen warns them.
> **Maintainer:** the repository owner; sole approver of releases.
> **Privacy:** Chitra has no telemetry and links no networking code; it uses
> the system `curl` only for an opt-in daily update check and for model
> downloads the user starts.

### Code signing policy (to publish on the project page once signed)

- Only builds made by the project's GitHub Actions workflow from the tagged
  source in this repository are signed. Nothing built on a developer machine is
  signed.
- Releases are approved by the repository owner, who is also the only committer
  and reviewer.
- Privacy: see the Security section of the README.

README attribution to add after approval:

> Free code signing provided by [SignPath.io](https://signpath.io),
> certificate by [SignPath Foundation](https://signpath.org).

## CI changes (after approval)

In the `windows-release` job: upload the unsigned `chitra.exe`, `chitra-cli.exe`
and the installer as an artifact, call `signpath/github-action-submit-signing-request`
(waiting for the result), download the signed files, and verify them in the
smoke test with `Get-AuthenticodeSignature` (status `Valid`). The release job
then publishes the signed files.

## Alternatives

If SignPath declines: Azure Artifact Signing (about $10 a month, identity
check, works from GitHub Actions) or a commercial certificate on a hardware
token or cloud HSM (about $200 to $500 a year). A self-signed certificate does
not remove the warning for other people.
