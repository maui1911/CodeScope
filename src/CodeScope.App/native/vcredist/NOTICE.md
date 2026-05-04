# Bundled Microsoft Visual C++ Runtime (x64)

These DLLs are shipped **app-local** beside `CodeScope.exe` so the native
renderer used by `EasyWindowsTerminalControl` →
`Microsoft.Terminal.Wpf` → `Microsoft.Terminal.Control.dll` (in
`runtimes/win-x64/native/`) loads on machines that do **not** have the
Microsoft Visual C++ 2015–2022 Redistributable installed.

Without these, fresh-install users see a black workspace and no terminal
ever initialises (the native control silently fails to load → the
`HwndHost` stays empty). See ADR-0017 / PR #51 for the full story.

## Files

| File                | Version        | SHA-256                                                            | Size    |
|---------------------|----------------|--------------------------------------------------------------------|---------|
| `vcruntime140.dll`  | 14.50.35719.0  | `184146852727a9db4eea06178716bec3cdbb1015c911f6b0f915b184ad7775b2` | 121 KB  |
| `vcruntime140_1.dll`| 14.50.35719.0  | `e6bfb3662ab4b1969a73441dbe35c96d51441b6bff8cf1fe7430bd5b246ca605` |  47 KB  |
| `msvcp140.dll`      | 14.50.35719.0  | `def46aa6a8f72f27bafac0c43334419486a4d1dcdb6c479a8ef7034b3e1fa4cb` | 541 KB  |

Total: ~709 KB.

## Source

Pulled from `C:\Windows\System32\` on a developer machine where the latest
Microsoft Visual C++ Redistributable was installed. This produces bit-identical
binaries to the official redist payload — Microsoft updates System32 from the
same MSI.

To refresh, run:

```pwsh
pwsh tools/refresh-vcruntime.ps1
```

The script copies the three DLLs from `%SystemRoot%\System32`, prints the
new versions and SHA-256 hashes, and updates the table above is *not*
automatic — bump the table by hand after a refresh.

## Redistribution license

These three DLLs are listed in the **Distributable Code** section of the
Visual Studio 2022 license (and equivalent for Build Tools / Community), which
explicitly grants the right to redistribute them with applications that
target the platform. See:

- <https://learn.microsoft.com/en-us/visualstudio/releases/2022/redistribution>
- <https://learn.microsoft.com/en-us/cpp/windows/redistributing-visual-cpp-files>

We ship only the three DLLs the terminal renderer actually needs — no MFC,
no OpenMP, no Concurrency Runtime extras. The complete x64 redistributable
itself (`vc_redist.x64.exe`, ~25 MB) is **not** bundled; users who want the
system-wide install can grab it from <https://aka.ms/vs/17/release/vc_redist.x64.exe>.
