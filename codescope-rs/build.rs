//! Build script — embeds the Windows application icon into the
//! `window` binary. On non-Windows hosts this is a no-op.
//!
//! Mirrors `<ApplicationIcon>assets\codescope.ico</ApplicationIcon>`
//! from the C# build's `CodeScope.App.csproj`. The resource script
//! at `assets/codescope.rc` references `codescope.ico` (in the same
//! folder) under resource ID `1`, which is what Windows looks up
//! when it needs "the" icon for an executable.

fn main() {
    #[cfg(windows)]
    embed_resource::compile("assets/codescope.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("compile codescope.rc");
}
