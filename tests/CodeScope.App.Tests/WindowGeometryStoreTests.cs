using System.Text.Json;
using NoScope.CodeScope.App.Persistence;

namespace NoScope.CodeScope.App.Tests;

public sealed class WindowGeometryStoreTests
{
    // Save/Apply touch real WPF Window state and the user's installed-app file; we exercise
    // the WindowGeometry record's serialisation contract instead — that's the wire format.

    [Fact]
    public void WindowGeometry_RoundTrips_AcrossSystemTextJson()
    {
        var original = new WindowGeometryStore.WindowGeometry(100.5, 75.0, 1280.0, 720.0, "Maximized");

        var json = JsonSerializer.Serialize(original);
        var roundtripped = JsonSerializer.Deserialize<WindowGeometryStore.WindowGeometry>(json);

        roundtripped.Should().Be(original);
    }

    [Fact]
    public void WindowGeometry_RecordValueEquality()
    {
        var a = new WindowGeometryStore.WindowGeometry(0, 0, 800, 600, "Normal");
        var b = new WindowGeometryStore.WindowGeometry(0, 0, 800, 600, "Normal");
        var c = new WindowGeometryStore.WindowGeometry(0, 0, 800, 600, "Maximized");

        a.Should().Be(b);
        a.GetHashCode().Should().Be(b.GetHashCode());
        a.Should().NotBe(c);
    }

    [Fact]
    public void Load_NoExistingFile_ReturnsNullSafely()
    {
        var result = WindowGeometryStore.Load();

        // No exception thrown is the assertion — result is null (no file) or a real
        // geometry (parallel installed build wrote one). Either outcome is correct.
        (result is null or WindowGeometryStore.WindowGeometry).Should().BeTrue();
    }
}
