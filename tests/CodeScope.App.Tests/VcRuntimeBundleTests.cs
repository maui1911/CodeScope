using System.IO;
using System.Reflection;

namespace NoScope.CodeScope.App.Tests;

/// <summary>
/// Guards the app-local Visual C++ runtime bundle. Microsoft.Terminal.Wpf's
/// native renderer (Microsoft.Terminal.Control.dll) silently fails to load on
/// machines without the VC++ Redistributable, leaving users with a black
/// workspace and no terminals — see src/CodeScope.App/native/vcredist/NOTICE.md
/// for the full context. These tests prevent silent regressions if the
/// Content glob is ever removed from CodeScope.App.csproj.
/// </summary>
public sealed class VcRuntimeBundleTests
{
    private static readonly string[] RequiredDlls =
    [
        "vcruntime140.dll",
        "vcruntime140_1.dll",
        "msvcp140.dll",
    ];

    private static string AppOutputDir
    {
        get
        {
            // Test assemblies live under tests/CodeScope.App.Tests/bin/<cfg>/<tfm>/.
            // The app drops next to it under src/CodeScope.App/bin/<cfg>/<tfm>/.
            var testDir = Path.GetDirectoryName(typeof(VcRuntimeBundleTests).Assembly.Location)!;
            var tfm = Path.GetFileName(testDir);
            var cfg = Path.GetFileName(Path.GetDirectoryName(testDir)!);
            var repoRoot = Path.GetFullPath(Path.Combine(testDir, "..", "..", "..", "..", ".."));
            // App TFM differs from test TFM (windows10.0.19041.0 vs windows). Resolve via glob.
            var appBin = Path.Combine(repoRoot, "src", "CodeScope.App", "bin", cfg);
            if (!Directory.Exists(appBin))
            {
                return appBin; // let the test fail with a clear path
            }
            var match = Directory.EnumerateDirectories(appBin)
                .FirstOrDefault(d => Path.GetFileName(d).StartsWith("net", StringComparison.Ordinal));
            return match ?? Path.Combine(appBin, tfm);
        }
    }

    [Theory]
    [InlineData("vcruntime140.dll")]
    [InlineData("vcruntime140_1.dll")]
    [InlineData("msvcp140.dll")]
    public void RuntimeDll_Is_Copied_Next_To_CodeScope_Exe(string dllName)
    {
        var path = Path.Combine(AppOutputDir, dllName);
        File.Exists(path).Should().BeTrue(
            $"{dllName} must ship app-local so Microsoft.Terminal.Control.dll loads on machines " +
            "without the Visual C++ Redistributable. Check the Content glob in CodeScope.App.csproj " +
            "and the source DLLs under src/CodeScope.App/native/vcredist/.");
    }

    [Fact]
    public void Source_Dlls_Exist_In_Repo()
    {
        var testDir = Path.GetDirectoryName(typeof(VcRuntimeBundleTests).Assembly.Location)!;
        var repoRoot = Path.GetFullPath(Path.Combine(testDir, "..", "..", "..", "..", ".."));
        var src = Path.Combine(repoRoot, "src", "CodeScope.App", "native", "vcredist");
        foreach (var name in RequiredDlls)
        {
            File.Exists(Path.Combine(src, name)).Should().BeTrue(
                $"{name} must be committed at native/vcredist/. Run tools/refresh-vcruntime.ps1 " +
                "on a machine with the latest VC++ Redistributable to regenerate.");
        }
    }
}
