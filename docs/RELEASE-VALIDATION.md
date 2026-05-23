# Release validation checklist

Run this before announcing any `vX.Y.Z` tag. The auto-update path
was previously responsible for three release-cycle crashes (rc.10 /
rc.12 / rc.14 — see HANDOFF session 44); the unit tests alone never
caught the crashes that mattered, so this manual end-to-end pass is
mandatory.

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

### 6. Dev-mode end-to-end (optional, but recommended on contentful releases)

For a self-contained dry run without publishing:

```pwsh
# 1. Stage a fake archive at version 99.0.0 served locally
cargo build --release --bin codescope
New-Item -ItemType Directory -Path dist/fake -Force | Out-Null
Copy-Item target/release/codescope.exe dist/fake/CodeScope.exe
Compress-Archive -Path dist/fake/* `
  -DestinationPath "dist/CodeScope-v99.0.0-windows.zip" -Force
python -m http.server 8000 --directory dist  # leave running

# 2. Launch the dev build with the override pointing at the local zip
$env:CODESCOPE_DEV = "1"
$env:CODESCOPE_DEV_FAKE_UPDATE_TOAST = "1"
$env:CODESCOPE_DEV_UPDATE_URL = "http://127.0.0.1:8000/CodeScope-v99.0.0-windows.zip"
cargo run --bin codescope
```

- [ ] Toast appears.
- [ ] Click "Update". Download proceeds. Installing succeeds.
- [ ] Restart prompt appears.
- [ ] Restart, re-launch — same binary (it's the dev build pointed
      at itself), but the entire flow ran through without crashing.

### Sign-off

When all of 1-5 pass, the release is OK to announce. Push the
release notes to the GitHub release body, mention any breaking
changes, and link relevant PRs.
