# Open-source release pipeline + install/update scripts — Design

Date: 2026-07-11
Status: approved

## Goal

Make winmux open-source ready: a tag-driven GitHub Actions release pipeline, a
single-command PowerShell installer that puts `winmux` on the user's PATH, an
update path, and the supporting repo hygiene (CI, README refresh, Cargo.toml
metadata). Zero changes to the Rust binary.

## Decisions (user-approved)

- **Release trigger:** tag-push driven (`v*` tags).
- **Install dir:** `%LOCALAPPDATA%\Programs\winmux` (per-user, no admin).
- **Update UX:** idempotent installer + a `winmux-update.ps1` stub dropped into
  the install dir; the stub fetches and runs the canonical `install.ps1` from
  `main` so update logic never goes stale.
- **Extras in scope:** CI workflow on push/PR, README refresh, Cargo.toml
  metadata + release profile.
- **Out of scope (YAGNI):** crates.io publishing, winget/scoop manifests, code
  signing, ARM64 builds, a built-in `winmux update` subcommand, CONTRIBUTING.md.

## Components

### 1. Cargo.toml

Add `description`, `license = "MIT"`, `repository`, `keywords`, `categories`,
and a `[profile.release]` block (`lto = true`, `strip = true`,
`codegen-units = 1`).

### 2. `.github/workflows/ci.yml`

Push/PR to `main`, on `windows-latest` (ConPTY + named pipes available):
checkout → stable toolchain + clippy → `Swatinem/rust-cache` →
`cargo clippy --all-targets -- -D warnings` →
`cargo test -- --test-threads=4` (the flake-aware thread cap from CLAUDE.md —
hosted runners are exactly the "loaded machine" contention case).

### 3. `.github/workflows/release.yml`

On `v*` tag push:

1. Guard: tag version must equal `Cargo.toml` version.
2. Gate: same clippy + test steps as CI.
3. `cargo build --release`.
4. Package `winmux-vX.Y.Z-x86_64-pc-windows-msvc.zip` (containing
   `winmux.exe`) + `SHA256SUMS`.
5. `gh release create` with `--generate-notes` and both assets (built-in `gh`
   CLI, no third-party release action; needs `contents: write`).

Release procedure: bump version in Cargo.toml, commit, tag `vX.Y.Z`, push tag.

### 4. `install.ps1` (repo root — short raw URL)

    irm https://raw.githubusercontent.com/ebonian/winmux/main/install.ps1 | iex

Works under Windows PowerShell 5.1 and PowerShell 7. Flow:

1. Resolve target release via the GitHub API (`releases/latest`), or a pinned
   `-Version vX.Y.Z` when run as a file. `-InstallDir` override for testing.
2. Idempotence: a `version.txt` marker in the install dir records the
   installed version; if it matches the target, print "already up to date"
   and exit (install = update).
3. Download the zip asset, verify SHA256 against the release's `SHA256SUMS`.
4. Locked-exe handling: if the installed `winmux.exe` is locked (a server or
   client is running), abort with a clear "run `winmux kill-server` first"
   message. Never auto-kill user sessions.
5. Extract to the install dir, write `version.txt`, drop the
   `winmux-update.ps1` stub.
6. PATH: append the install dir to the **user** PATH via the registry
   (preserving the value kind, not `Environment.SetEnvironmentVariable`,
   which would expand `%VAR%` entries), broadcast `WM_SETTINGCHANGE`, and
   patch `$env:Path` in the current session.

### 5. Update: `winmux-update`

The stub in the install dir (on PATH, so `winmux-update` just works in
PowerShell) fetches and runs the latest `install.ps1`; the installer's
version check makes it a no-op when already current.

### 6. README refresh

Rewrite the stale MVP-era README for the finished project: badges (CI,
release, license), install one-liner up top, features through SP7, current
keybindings, config-file notes, update/uninstall, build-from-source, docs
links.

## Testing

- CI validates itself on the push; the release workflow is validated by
  cutting `v0.1.0` as the first real release.
- Installer smoke-tested locally against the published release (custom
  `-InstallDir`), then user-tested with the real one-liner: install → PATH →
  `winmux` runs → `winmux-update` reports up to date → locked-exe path.
