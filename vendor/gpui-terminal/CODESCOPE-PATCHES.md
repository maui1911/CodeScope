# CodeScope-rs local patches to `gpui-terminal`

Vendored from <https://github.com/zortax/gpui-terminal> (MIT OR Apache-2.0)
at commit `main` (cloned 2026-05-08). CodeScope-rs depends on this
directory via a `path = "vendor/gpui-terminal"` dependency. The patches
below live only here; we do not maintain a fork on GitHub and do not
intend to send them upstream until the project's maintainer signals
interest.

## Patch — forward `Event::PtyWrite` to the PTY

**Files:** `src/event.rs`, `src/view.rs`

**Symptom (Windows, May 2026):** spawning `cmd.exe` or `pwsh.exe` inside
the terminal hangs at startup. The shell sends `ESC[6n` (Device Status
Report — cursor position request) and waits for the response
`ESC[<row>;<col>R` before printing its prompt. With the unpatched library
no response ever arrives, so the terminal stays empty.

**Root cause:** `GpuiEventProxy::send_event` matched
`Event::PtyWrite(ref _data) => { /* "handled internally by alacritty" */ }`.
That comment is wrong. `Event::PtyWrite` is alacritty's mechanism for
asking the embedder to write specific bytes _back_ to the PTY (DSR
responses, primary/secondary device-attribute replies, etc.). Dropping
those payloads breaks any program that performs a query/response handshake
on startup.

**Fix:** thread the existing `Arc<Mutex<Box<dyn Write + Send>>>` PTY-stdin
writer (already owned by `TerminalView`) into `GpuiEventProxy`. In
`send_event`, the `Event::PtyWrite(data)` arm now calls
`writer.write_all(data.as_bytes())` directly. Construction order in
`TerminalView::new` is reordered so the writer is wrapped before the
proxy is built. Test helper `sink_writer()` added to keep `event.rs`'s
unit tests compiling. ~30-line diff total.

This _does not_ fix the conhost-vs-grid scroll-sync issue (Bug #2 in
`docs/HANDOFF.md`); that requires architectural changes outside this
crate's surface and is out of scope for the spike.
