# Auto-update (manual-apply) + branding cleanup — design

**Date:** 2026-05-22
**Status:** draft — awaiting user review
**Related:** PR #244 (auto-update rip), session 44 HANDOFF (velopack post-mortem), ADR-0022 (C# → Rust)

## Problem

Auto-update was ripped from CodeScope in PR #244 after crashing on three consecutive release cycles (rc.10 / rc.12 / rc.14). Root cause was velopack-rs's `VelopackApp::run()` calling `call_fast_hook`, which unconditionally `exit(0)`s — combined with `Update.exe` re-launching the new binary with `--veloapp-updated` / `--veloapp-install` argv flags that the app couldn't recover from. The user lost ~two sessions of work to this loop.

Net of #244: no in-app update surface at all. Users upgrade by going to GitHub Releases, downloading the MSI, running it. CodeScope is shipped as cargo-dist MSI (Windows) + `.tar.xz` (macOS / Linux), with `install-updater = false` to keep axoupdater from creeping back in.

The user wants update awareness back, but with two firm constraints:

1. **No silent apply.** The app must surface "update available" but the user explicitly clicks an "Update" button to download + install. No background apply, no surprise restarts.
2. **No velopack.** Whatever engine we pick must not replicate the hook-exit + relaunch-flag pattern that bit us three times.

Alongside the updater work, the user wants the user-visible `codescope-rs` branding cleaned up — install dir, executable filename, MSI product name, Start Menu shortcut, release asset filenames. The `-rs` suffix is a leftover from the C# build retirement that's no longer load-bearing.

## Scope

In:

- Background poll of GitHub Releases (3-hour interval + once at startup), comparing against `env!("CARGO_PKG_VERSION")`.
- A toast + bell-popover entry "CodeScope vX.Y.Z is beschikbaar" with **Update** and **Later** actions when a newer release is found.
- On **Update**: download the platform archive, atomic-swap the running binary via the `self_update` crate, surface "Restart to activate" then `exit(0)`.
- On **Later**: dismiss the toast for this session; re-surface on next launch or next poll if still applicable.
- Windows packaging migration: **drop cargo-dist's MSI, replace with Inno Setup** producing `<App>-vX.Y.Z-setup.exe` + a sibling `.zip` archive (the `.zip` is the `self_update` target).
- Branding rename: `codescope-rs` → `CodeScope` across install path, executable, MSI / Inno product metadata, Start Menu shortcut, release asset filenames. Cargo workspace package name stays `codescope`.
- macOS = notifier-only. Toast surfaces the new release but the **Update** action opens the GitHub Releases page in the default browser (no in-app apply path).
- A `.iss` Inno Setup script checked into the repo at `installer/CodeScope.iss`.
- Release workflow rewrite: cargo-dist keeps building the binaries + tar.xz for macOS / Linux; Windows job adds `choco install innosetup` + `iscc` + zip-bundle steps.

Out (YAGNI):

- **Code signing (Windows + macOS).** v1 ships unsigned. SmartScreen will show "Windows protected your PC" once per fresh install until the user clicks through. Azure Trusted Signing (~$10/month) is the planned follow-up — infra is documented below but not implemented. macOS notarization ($99/year Apple Developer ID) follows the same pattern.
- **Microsoft Store / MSIX.** Incompatible with self_update (Store install dir is read-only), and dev-tool iteration cadence is incompatible with Store certification latency. Revisit only if there's a concrete distribution reason.
- **Delta updates.** `self_update` downloads the full archive. CodeScope binaries are <100 MB; full-download is acceptable.
- **Silent / scheduled apply.** Every apply requires explicit user click on the toast.
- **macOS in-app apply.** Atomic swap inside an `.app` bundle without notarization triggers Gatekeeper; deferred until Developer ID + notarization land.
- **Channels (stable / beta / canary).** One channel: the latest GitHub Release.
- **Rollback.** If a release breaks, the user installs a previous version from the Releases page manually.
- **In-app changelog rendering.** The toast just says "vX.Y.Z is beschikbaar"; users who want details click through to the release page.
- **Migration of existing `codescope-rs` MSI installs.** Clean break — release notes will say "uninstall the old `codescope-rs` first." User base is small enough (jij + handvol dogfooders) that this is acceptable.

## UX

### When a new release is detected

A toast appears in the bell-popover stack with severity `Info`:

```
┌──────────────────────────────────────────────────┐
│ ⓘ Update beschikbaar                             │
│                                                  │
│ CodeScope v0.3.1 is uit.                         │
│ Huidige versie: v0.3.0                           │
│                                                  │
│      [ Update ]   [ Later ]                      │
└──────────────────────────────────────────────────┘
```

- Toast persists in the bell-popover (does not auto-dismiss). The transient toast tray surfaces it once when first detected; subsequent polls during the same session that hit the same version do **not** re-fire the transient surface (mirrors session 44's `last_announced_update` field).
- **Update** triggers the download + apply flow (see next section).
- **Later** dismisses the toast for the current session; if the user is still on the older version next launch, it re-surfaces.

### When the user clicks **Update**

Toast updates in place to show progress:

1. `Downloading… (12.4 MB / 47.0 MB)` — driven by `self_update`'s progress callback.
2. `Installing…` — atomic swap in progress.
3. `Klaar — herstart om te activeren` with a single **Restart** button.
4. Click **Restart** → `exit(0)`. The user re-launches CodeScope from Start Menu / dock / shortcut. The new binary is in place.

If any step fails:

- Toast switches to severity `Error` with the failure message.
- A **Try again** button retries the download.
- The current binary is untouched (atomic swap means partial state never leaks).

### macOS-specific divergence

On macOS the toast says the same thing but the **Update** action does not download — it opens `https://github.com/maui1911/CodeScope/releases/latest` in the default browser. The user downloads the `.tar.xz`, extracts it, replaces their `.app` manually. Spec documents this is v1 behavior; full self_update on mac is a tracked follow-up issue gated on notarization.

## Architecture

### New / restored modules

```
core/src/update_check.rs        — restored (~300 lines, was 786)
├── struct ReleaseInfo { version: Version, archive_url: String, ... }
├── async fn check_latest() -> Result<Option<ReleaseInfo>>
└── (no apply logic here — apply lives in src/update.rs)

src/update.rs                   — new (~250 lines)
├── enum UpdateStatus { Idle, Checking, UpToDate, Available(ReleaseInfo),
│                       Downloading{progress}, Installing, Ready, Failed{err} }
├── struct UpdateState (Arc<RwLock<UpdateStatus>>)
├── fn check_in_background(state: UpdateState) -> JoinHandle
├── fn install_in_background(state: UpdateState, info: ReleaseInfo) -> JoinHandle
└── fn open_releases_page() -> Result<()>   (macOS apply fallback)
```

`update_check::check_latest()` uses the `self_update` crate's `backends::github::ReleaseList` to enumerate releases and pick the highest semver. We do **not** use `self_update`'s built-in `Update` driver for the apply step — we drive `self_update::self_replace::self_replace` directly so we own the progress reporting and error mapping.

`UpdateState` is `Arc<RwLock<UpdateStatus>>` — the simplest possible shared slot between the background thread and the GPUI render loop. Render reads a snapshot per frame; background thread holds the write lock only for the transition moments.

### Changes to existing modules

- `src/app.rs` — `AppShell` regains an `update_state: UpdateState` field. The `start_update_check_poll` method comes back, this time spawning a thread that loops `check_latest` every 3 hours and writes to the state slot. `ToastAction` / `ToastActionKind` come back as in session 44 (manual-apply gating already existed there).
- `src/notifications.rs` — add `NotificationKind::UpdateAvailable { version }`. (Not `Generic` — explicit semantics so we can route bell-popover entries correctly.)
- `src/main.rs` — no velopack imports, no boot-tape `velopack:*` markers. `windows_subsystem = "windows"` stays as-is.
- `Cargo.toml` (root) — drop `velopack` dep (already gone), add `self_update = { version = "0.41", features = ["archive-zip", "archive-tar", "compression-flate2"] }`. `ureq` returns as a transitive dep of `self_update` (was ~directly used by the old `update_check.rs`).
- `core/src/lib.rs` — re-export `update_check`.

### Data flow

```
App startup (src/main.rs)
  └─> AppShell::new
        └─> spawn check_in_background(state)
              └─> loop { check_latest; sleep 3h }
                    └─> on Available: write state slot

GPUI render tick (per frame)
  └─> AppShell::render
        └─> snapshot state slot
              └─> if Available && !announced: push UpdateAvailable toast
                    └─> Toast carries ToastAction::ApplyUpdate(info)

User clicks Update on toast
  └─> dispatch_toast_action(ApplyUpdate(info))
        └─> spawn install_in_background(state, info)
              └─> self_update::self_replace
                    └─> on success: state = Ready, toast → "Restart"

User clicks Restart
  └─> std::process::exit(0)
```

The background thread never calls into GPUI; GPUI reads via the state slot. No re-entrancy worries.

## Cross-platform matrix

| Platform | Initial install | Update target archive | Apply path | macOS note |
|---|---|---|---|---|
| Windows x64 | `CodeScope-vX.Y.Z-setup.exe` (Inno) | `CodeScope-vX.Y.Z-windows.zip` | `self_update` atomic swap | — |
| macOS arm64 | `CodeScope-vX.Y.Z-aarch64-apple-darwin.tar.xz` (cargo-dist) | n/a (v1) | Open Releases page | Gatekeeper would block unsigned swap |
| macOS x64 | `CodeScope-vX.Y.Z-x86_64-apple-darwin.tar.xz` (cargo-dist) | n/a (v1) | Open Releases page | Same |
| Linux x64 | `CodeScope-vX.Y.Z-x86_64-unknown-linux-gnu.tar.xz` (cargo-dist) | same `.tar.xz` | `self_update` atomic swap | — |

## Release pipeline changes

`.github/workflows/release.yml` (regenerated by cargo-dist + hand-edited):

```yaml
windows-job:
  runs-on: windows-2022
  steps:
    - cargo build --release --bin codescope
    - rename target/release/codescope.exe → CodeScope.exe
    - stage CodeScope.exe + LICENSE + CHANGELOG.md + README.md → dist/bundle/
    - Compress-Archive dist/bundle/* → CodeScope-v${TAG}-windows.zip
    - choco install -y innosetup
    - iscc /DMyAppVersion=${TAG} /O"dist" installer/CodeScope.iss
    - upload artifact: dist/CodeScope-v${TAG}-windows.zip + dist/CodeScope-v${TAG}-setup.exe

macos-arm64-job / macos-x64-job / linux-job:
  unchanged from current cargo-dist
  → CodeScope-v${TAG}-<triple>.tar.xz

publish-job:
  - gh release create v${TAG} <all four artifacts>
```

`dist-workspace.toml`:

- `installers = []` (was `["msi"]`)
- Drop `allow-dirty = ["msi"]` (no more WiX template)
- Keep `install-updater = false`
- Keep `targets`, `cargo-dist-version`, `hosting = "github"`, `source-tarball = false`, `pr-run-mode = "skip"`
- Strip the now-obsolete "MSI installs per-user via..." commentary block

`wix/main.wxs` — **deleted**. Same for the rationale block in `dist-workspace.toml` that describes it. The `[package.metadata.wix]` block in root `Cargo.toml` (`upgrade-guid` + `path-guid`) goes with it.

`installer/CodeScope.iss` — new, ~40 lines:

```ini
[Setup]
AppName=CodeScope
AppVersion={#MyAppVersion}
AppPublisher=maui1911
DefaultDirName={localappdata}\Programs\CodeScope
DefaultGroupName=CodeScope
PrivilegesRequired=lowest
OutputBaseFilename=CodeScope-v{#MyAppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SourceDir=..

[Files]
Source: "dist\bundle\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{group}\CodeScope"; Filename: "{app}\CodeScope.exe"
Name: "{userdesktop}\CodeScope"; Filename: "{app}\CodeScope.exe"; Tasks: desktopicon

[Tasks]
Name: desktopicon; Description: "Create a desktop icon"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Run]
Filename: "{app}\CodeScope.exe"; Description: "Launch CodeScope"; Flags: nowait postinstall skipifsilent
```

## Branding migration

| Surface | Was | Becomes |
|---|---|---|
| Cargo root package | `codescope-rs` | `codescope` |
| Cargo bin target | `codescope-rs` | `codescope` |
| Built executable | `codescope-rs.exe` | `codescope.exe`, renamed to `CodeScope.exe` during Windows staging |
| Install dir (Windows) | `%LOCALAPPDATA%\Programs\codescope-rs\bin\` | `%LOCALAPPDATA%\Programs\CodeScope\` |
| Apps & Features entry | `CodeScope (Rust)` / `CodeScope (Rust port)` | `CodeScope` |
| MSI ProductName / DiskPrompt | `codescope-rs` / `CodeScope (Rust port)` | n/a (MSI dropped) |
| Inno AppName | n/a | `CodeScope` |
| Start Menu folder | `CodeScope (Rust)` | `CodeScope` |
| Start Menu shortcut | `CodeScope (Rust)` | `CodeScope` |
| Release asset prefix | `codescope-rs-` | `CodeScope-v` |
| Member crates | `codescope-core`, `codescope-terminal` | unchanged (internal — not user-visible) |
| `paths::AppPaths` data dirs | `%APPDATA%\CodeScope\` | unchanged (already correct) |
| Window title | `CodeScope — …` | unchanged (already correct) |
| Single-instance mutex | `Global\CodeScope.SingleInstance` | unchanged |

The `(Rust)` / `(Rust port)` parentheticals currently live in `wix/main.wxs` (MSI `Manufacturer` description on line 75, `AppShortcutFolder` Name on line 200, shortcut Name on line 203, Description on line 204). Deleting the WiX template removes every instance in one move; the new Inno `.iss` uses `AppName=CodeScope` with no parenthetical, so Apps & Features will display just `CodeScope` after the next install.

Touch points in code:

- `Cargo.toml` (root): `[package] name = "codescope-rs"` → `name = "codescope"`, `[[bin]] name = "codescope-rs"` → `name = "codescope"`.
- `Cargo.lock` regenerates on next build — committed.
- `dist-workspace.toml`: see above.
- CI workflow: every reference to `codescope-rs.exe` becomes a build of `codescope.exe` followed by a rename-to-`CodeScope.exe` staging step on Windows.
- `README.md` install instructions — `codescope-rs` references → `CodeScope`.
- `CLAUDE.md` Velopack mandate block — remove (stale since #244).
- `docs/HANDOFF.md` — session 46 entry will document the rename.

The Cargo root package + bin name are lowercased (`codescope`) because Cargo conventions resist CamelCase. The *displayed* binary on Windows is renamed to `CodeScope.exe` during staging (`Move-Item target\release\codescope.exe CodeScope.exe` before zipping + Inno). On macOS / Linux the binary inside the tar.xz can also be `CodeScope` if we want — cleaner for `which`. The member crates (`codescope-core`, `codescope-terminal`) keep their hyphenated names; they're never user-visible and renaming them would churn every `use codescope_core::*` import in the workspace for zero value.

### Migration story for existing installs

v0.3.0 (current) installs to `%LOCALAPPDATA%\Programs\codescope-rs\` via MSI. v0.3.1 will install to `%LOCALAPPDATA%\Programs\CodeScope\` via Inno. These are entirely separate installs from Windows' perspective; no MSI UpgradeCode bridges them.

**Release notes for v0.3.1 will instruct:**

> If you're upgrading from v0.3.0 or earlier: uninstall "codescope-rs" from Add/Remove Programs first, then run the new `CodeScope-setup.exe`. Your projects, layout, and settings are preserved (data lives in `%APPDATA%\CodeScope\`, which the rename doesn't touch).

User-data paths (`%APPDATA%\CodeScope\`, `%LOCALAPPDATA%\CodeScope\`) were already `CodeScope`-named under `AppPaths` — no migration needed there.

## Testing strategy

The velopack post-mortem made clear we can't trust unit tests to catch the failure mode that actually bit us (startup `exit(0)` in a hook the app didn't even register). The new flow has to be **observable** end-to-end on a real machine, not just clean in unit tests.

### Unit tests (core / `update_check`)

`core/src/update_check.rs` tests (mirror what existed pre-#244):

1. **Version-newer detection** — feed a synthetic Release JSON with a higher semver, expect `Some(ReleaseInfo)` back.
2. **Version-equal / older** — expect `None`.
3. **Pre-release tag handling** — `v0.3.1-rc.1` vs `v0.3.0`: rc gets surfaced; vs `v0.3.1` stable: rc does **not** get surfaced. (User has used the `rc.N` cadence; we don't want stable users seeing rc updates.)
4. **Archive URL selection** — given a multi-asset Release, the matching platform archive URL is returned for current `target_triple()`.
5. **Network failure** — mock a transport error, expect `Err` (not panic).
6. **Malformed JSON** — expect `Err`.

### Integration tests (manual end-to-end)

Add `docs/RELEASE-VALIDATION.md` — a checklist run on each `vX.Y.Z` tag before announcing:

1. Fresh Windows VM: download `-setup.exe`, install, launch, verify `%LOCALAPPDATA%\Programs\CodeScope\CodeScope.exe` exists and runs.
2. Trigger a fake-newer-version path via env (`CODESCOPE_DEV_FAKE_UPDATE_TOAST=1`) — same env we used for session 44 — to surface the toast in dev mode.
3. Drop a hand-built newer-version zip into a local HTTP server, point `CODESCOPE_DEV_UPDATE_URL` at it, click **Update**, verify atomic swap + restart works.
4. Linux VM equivalent with tar.xz.
5. macOS: verify toast surfaces, **Update** opens the Releases page, browser opens correctly.

### What we will *not* rely on

- Unit-testing the apply path. `self_update::self_replace` is small and well-trodden; we trust its tests.
- Testing the GitHub Releases API integration without hitting it once on each release. The first real-release validation is mandatory.

## Follow-up issues (out of scope for this spec)

To open as separate GitHub issues after this lands:

1. **Code signing (Windows).** Wire up Azure Trusted Signing. `signtool sign` runs in CI on both `CodeScope.exe` (the binary inside the zip, so atomic-swap installs a signed exe) and `CodeScope-setup.exe` (the installer). Identity track: individual. Cost: ~$10/month + 3-5 day identity validation.
2. **macOS notarization.** Apple Developer ID ($99/year). cargo-dist supports notarization via `Apple Developer ID Application` cert; once enabled, mac graduates from notifier-only to full self_update apply path.
3. **In-app changelog rendering.** Pull the release body from the GitHub Releases API and render it in a modal before the user clicks **Update**.
4. **Microsoft Store listing.** Only if a concrete distribution need surfaces. Would be a secondary channel; Inno + self_update remains primary.

## Risks & known limitations

- **SmartScreen on first launch (Windows).** Unsigned installer triggers "Windows protected your PC." User clicks "More info" → "Run anyway." This is the accepted v1 trade-off; signing follow-up fixes it.
- **Single-instance mutex during apply.** Atomic swap renames `CodeScope.exe` to `CodeScope.exe.old` then writes the new exe. If the app holds the mutex the whole time and `exit(0)`s after writing, the mutex is released on process exit and the user can launch the new binary cleanly. Verified by `self_update` 's docs but worth confirming in step 3 of integration testing.
- **No rollback path.** If v0.3.5 ships broken, users have to manually re-install v0.3.4 from the Releases page. Same trade-off the project has today.
- **Mac is two flows.** Notifier-only on mac vs full apply on Win/Linux. Documented but means we test two surface areas. Accepted because the alternative (block the whole feature on Apple Developer ID + notarization) holds Windows / Linux users hostage.

## Open questions

None — all decisions locked during brainstorming session 2026-05-22.
