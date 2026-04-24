using System.Diagnostics;
using System.Text;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Shared invocation helper for external CLIs (git, gh, tea). Collapses three
/// near-identical <c>ProcessStartInfo</c> + read-stdout-stderr + Result-mapping
/// blocks into a single call site. Each caller supplies its executable path, a
/// log label (e.g. "git"), and a logger — everything else is uniform:
/// <list type="bullet">
///   <item>no shell (<c>UseShellExecute = false</c>, <c>CreateNoWindow = true</c>),</item>
///   <item>UTF-8 stdout/stderr with trimming,</item>
///   <item>non-zero exit → <see cref="Result{T}.Fail"/>,</item>
///   <item>missing executable → <see cref="Result{T}.Fail"/> "<i>tool</i> not found on PATH".</item>
/// </list>
/// </summary>
public static class ProcessRunner
{
    /// <summary>
    /// Runs <paramref name="executable"/> with <paramref name="args"/> in <paramref name="cwd"/>
    /// (null/empty for caller's cwd) and returns stdout trimmed on success, a formatted failure
    /// string otherwise. Never throws for normal exit-code / missing-tool paths.
    /// </summary>
    public static async Task<Result<string>> RunAsync(
        string executable,
        string? cwd,
        string args,
        string toolLabel,
        ILogger logger,
        CancellationToken ct)
    {
        var psi = new ProcessStartInfo(executable, args)
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
            StandardOutputEncoding = Encoding.UTF8,
            StandardErrorEncoding = Encoding.UTF8,
        };
        if (!string.IsNullOrEmpty(cwd))
        {
            psi.WorkingDirectory = cwd;
        }

        try
        {
            using var process = Process.Start(psi)
                ?? throw new InvalidOperationException($"Process.Start returned null for {toolLabel}");

            var stdoutTask = process.StandardOutput.ReadToEndAsync(ct);
            var stderrTask = process.StandardError.ReadToEndAsync(ct);

            await process.WaitForExitAsync(ct).ConfigureAwait(false);
            var stdout = (await stdoutTask.ConfigureAwait(false)).Trim();
            var stderr = (await stderrTask.ConfigureAwait(false)).Trim();

            if (process.ExitCode != 0)
            {
                logger.LogWarning("{Tool} {Args} exited {ExitCode}: {StdErr}", toolLabel, args, process.ExitCode, stderr);
                return Result<string>.Fail($"{toolLabel} {args} exited {process.ExitCode}: {stderr}");
            }

            return Result<string>.Ok(stdout);
        }
        catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or FileNotFoundException)
        {
            logger.LogError(ex, "{Tool} executable not found", toolLabel);
            return Result<string>.Fail($"{toolLabel} not found on PATH: {ex.Message}");
        }
    }

    /// <summary>
    /// Shell-quotes a free-text argument for a Windows <c>ProcessStartInfo</c> command line.
    /// Wraps in double quotes and escapes any inner double quotes. Used for PR title/body
    /// strings passed to gh/tea where the arg contains spaces.
    /// </summary>
    public static string QuoteArg(string value) => $"\"{value.Replace("\"", "\\\"")}\"";

    /// <summary>
    /// Picks the last http(s) URL emitted in a multi-line CLI output block — both
    /// <c>gh pr create</c> and <c>tea pulls create</c> append the created PR URL as the
    /// last line after some chatter. Scans back-to-front and returns the first line that
    /// starts with <c>http://</c> or <c>https://</c>, or <c>null</c> when none matches.
    /// </summary>
    public static string? ExtractLastUrl(string output)
    {
        foreach (var line in output.Split('\n').Reverse())
        {
            var trimmed = line.Trim();
            if (trimmed.StartsWith("http://", StringComparison.Ordinal) || trimmed.StartsWith("https://", StringComparison.Ordinal))
            {
                return trimmed;
            }
        }
        return null;
    }
}
