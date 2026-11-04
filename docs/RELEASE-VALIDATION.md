# Release validation checklist

Run this before announcing any `vX.Y.Z` tag. The auto-update path
crashed three release cycles in a row — rc.10, rc.12 and rc.14 — and
that is what retired Velopack in #244. The unit tests caught none of
them, so the real in-app update (§4) stays a manual step on every
release.

The localhost dry runs of the updater (archive extraction, truncated
downloads, the GitHub API asset URL) were dropped once the updater had
held up across several real releases. Their failure modes are pinned by
unit tests in `src/update.rs` (the `verify_complete` tests and
`download_asks_github_for_the_raw_asset_bytes`); the old steps live in
this file's git history if the updater changes again.

## Per release

### 1. Fresh-install validation (Windows)

- [ ] Download `CodeScope-vX.Y.Z-setup.exe` from the GitHub release.
- [ ] Run it on a Windows VM with no prior CodeScope install.
- [ ] Verify `%LOCALAPPDATA%\Programs\CodeScope\CodeScope.exe` exists.
- [ ] Apps & Features shows "CodeScope" (no "(Rust)" suffix).
- [ ] Start Menu has a "CodeScope" entry (no "(Rust)" suffix).
- [ ] Launch the app, open the project picker, add a project, spawn
      a session. App stays up.

### 2. Fresh-install validation (Linux)

- [ ] Download `CodeScope-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`.
- [ ] Extract, run the binary on a Linux VM. App launches.

### 3. Fresh-install validation (macOS)

- [ ] Download the matching tar.gz for your arch.
- [ ] Extract, drag CodeScope to /Applications, launch.
- [ ] (Until notarization lands: Gatekeeper warns once; right-click
      → Open clears it.)

### 4. In-app update flow (Windows + Linux)

Tag `vX.Y.Z` and after the artifacts are published:

- [ ] Install `v(X.Y.Z-1)` from the previous release.
- [ ] Launch it.
- [ ] Wait ≤20 seconds for the first poll (15s startup delay +
      network), OR set `CODESCOPE_DEV_FAKE_UPDATE_TOAST=1` before
      launch to bypass.
- [ ] Verify the update toast appears in the bottom-right with
      "CodeScope vX.Y.Z is available" and an "Update" button.
- [ ] Verify the bell-popover has a matching "Update available"
      entry.
- [ ] Click "Update". Toast switches to a download/install state,
      then to "Update installed" with a "Restart" button.
- [ ] Click "Restart". The app exits.
- [ ] Re-launch from Start Menu / dock. Verify the title bar /
      About dialog shows vX.Y.Z.

### 5. In-app notifier flow (macOS)

- [ ] Install `v(X.Y.Z-1)` on macOS.
- [ ] Launch and wait for the toast.
- [ ] Click "Update". Verify it opens the GitHub release page in
      the default browser. (No in-app apply on macOS yet — by
      design until notarization lands.)

### 6. README still current (minor releases)

The README drifted two minor versions once (#344): a stale keyboard
table and features that shipped without a mention. On every minor
release (`vX.Y.0`):

- [ ] Every feature merged since the last minor release is findable in
      `README.md`.
- [ ] The keyboard table matches the chord table in `src/app.rs`
      (`AppShell::on_key_down`) and
      `codescope_terminal::view::is_app_level_shortcut`.
- [ ] Asset names in the Install table match the published release.
- [ ] `docs/screenshots/` still shows the current UI; reshoot stale ones
      in one sitting (same theme, window size and DPI).

### Sign-off

When all checks (1-6) pass, the release is OK to announce. Push the
release notes to the GitHub release body, mention any breaking
changes, and link relevant PRs.
