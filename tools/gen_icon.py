"""Render the CodeScope brand (rounded blue square + top-right cutout) to a
multi-size .ico. Mirrors the XAML mark in MainWindow.xaml (14x14 at 4px radius
with a 4px cutout at 3,3 from the top-right)."""
from PIL import Image, ImageDraw

ACCENT = (0, 153, 255, 255)   # #0099FF
CUTOUT = (0, 0, 0, 0)         # transparent so the icon reads on light/dark

SIZES = [256, 128, 96, 64, 48, 32, 24, 16]

def render(size: int) -> Image.Image:
    # Upscale then downscale for smooth antialiasing.
    scale = 4
    S = size * scale
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Match XAML: 14x14 canvas, CornerRadius=4, cutout 4x4 at margin (0,3,3,0)
    # from top-right with 1px rounded corners. Translate to current size.
    radius = int(round(S * 4 / 14))
    d.rounded_rectangle((0, 0, S - 1, S - 1), radius=radius, fill=ACCENT)

    # Cutout: 4x4 at top-right with margin 3 from right/top, rounded 1px.
    cw = int(round(S * 4 / 14))
    cmr = int(round(S * 3 / 14))   # margin from right
    cmt = int(round(S * 3 / 14))   # margin from top
    cr = max(1, int(round(S * 1 / 14)))
    x1 = S - cmr - cw
    y1 = cmt
    x2 = S - cmr
    y2 = cmt + cw
    d.rounded_rectangle((x1, y1, x2, y2), radius=cr, fill=CUTOUT)

    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    images = [render(s) for s in SIZES]
    out = r"C:\dev\codescope\src\CodeScope.App\assets\codescope.ico"
    import os
    os.makedirs(os.path.dirname(out), exist_ok=True)
    images[0].save(out, format="ICO", sizes=[(s, s) for s in SIZES], append_images=images[1:])
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
