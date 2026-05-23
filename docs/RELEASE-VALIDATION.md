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

### 6. Dev-mode end-to-end + archive-extraction regression (MANDATORY)

This is not optional. The in-app updater downloads an archive and
extracts it before the atomic swap. The download succeeding does NOT
mean the extraction will: archive extraction depends on `self_update`
*compression* feature flags being enabled in `Cargo.toml`, and a
download can complete only to fail at extract with "unsupported
extraction method". This bit us once already — `compression-zip-deflate`
was missing, so every `Compress-Archive` / cargo-dist zip (all DEFLATE-
compressed) would have failed to extract in production. No unit test
or PR review caught it; only clicking through the real flow did.

So the load-bearing check here is: **the flow must reach "Installing"
→ "Update installed", which proves extraction succeeded** — not just
"the download started".

For a self-contained dry run without publishing a real release:

```pwsh
# 1. Stage a fake "newer" archive at version 99.0.0.
#    Use the SAME tool the release workflow uses (Compress-Archive),
#    so the test exercises the real compression method, not a hand-
#    rolled STORED zip that would mask a missing deflate feature.
cargo build --release --bin codescope
New-Item -ItemType Directory -Path dist/fake -Force | Out-Null
Copy-Item target/release/codescope.exe dist/fake/CodeScope.exe
Compress-Archive -Path dist/fake/* `
  -DestinationPath "dist/CodeScope-v99.0.0-windows.zip" -Force

# 2. Serve it. A THROTTLED server (vs. `python -m http.server`) makes
#    the byte-progress toast visibly tick instead of completing
#    instantly over localhost — needed to eyeball the "N / M MB"
#    progress. See dist/slow_server.py in PR #249's history, or any
#    rate-limited static server on :8000.
python dist/slow_server.py  # leave running; serves at ~512 KB/s

# 3. Launch the DEBUG dev build with the override pointing at the zip.
#    (A release build ignores CODESCOPE_DEV_UPDATE_URL — it's gated
#    behind debug_assertions as a security measure.)
$env:CODESCOPE_DEV = "1"
$env:CODESCOPE_DEV_FAKE_UPDATE_TOAST = "1"
$env:CODESCOPE_DEV_UPDATE_URL = "http://127.0.0.1:8000/CodeScope-v99.0.0-windows.zip"
cargo run --bin codescope
```

- [ ] Toast appears (~15s after launch).
- [ ] Click "Update". The toast switches to "Downloading update… N / M"
      and the byte count **visibly advances** (throttled server).
- [ ] **Extraction succeeds:** the toast reaches "Installing update…"
      then "Update installed". (A failure here with "unsupported
      extraction method" means a `self_update` compression feature is
      missing for that archive format — fix `Cargo.toml`, do not ship.)
- [ ] Restart prompt appears; clicking "Restart" closes the app
      (`cx.quit()`).
- [ ] Re-launch — the dev binary was self-replaced and the entire flow
      ran through without crashing. Run `cargo build --bin codescope`
      afterward to relink a proper debug binary (self_replace left a
      release binary at `target/debug/`).

> Repeat the extraction check on Linux with a real `.tar.gz` (the
> `compression-flate2` feature) whenever the `self_update` feature set
> or `dist-workspace.toml`'s `unix-archive` changes.

### Sign-off

When all of 1-6 pass, the release is OK to announce. Push the
release notes to the GitHub release body, mention any breaking
changes, and link relevant PRs.
