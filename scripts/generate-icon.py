#!/usr/bin/env python3
"""Generate Resources/AppIcon.icns from a square source logo.

Places the logo on Apple's macOS icon grid: an 824x824 rounded body centered
on a 1024x1024 transparent canvas, with a soft drop shadow. The rounded mask
clips the source logo's opaque corners so the final icon has clean transparency.

Usage: /usr/bin/python3 scripts/generate-icon.py <source.png> [output.icns]
"""
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

# Apple macOS Big Sur icon grid (proportions of the 1024 canvas).
CANVAS = 1024
BODY = 824                      # rounded-rect body size
RADIUS = int(BODY * 0.2247)     # ~185px corner radius on the body
MARGIN = (CANVAS - BODY) // 2   # 100px padding around the body
SHADOW_BLUR = 11
SHADOW_OFFSET = 8
SHADOW_ALPHA = 70

ICONSET_SIZES = [16, 32, 64, 128, 256, 512, 1024]
# (filename, pixel size) pairs iconutil expects.
ICONSET_FILES = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
]


def rounded_mask(size, radius):
    mask = Image.new("L", (size, size), 0)
    draw = ImageDraw.Draw(mask)
    draw.rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=255)
    return mask


def build_master(src_path):
    logo = Image.open(src_path).convert("RGBA")
    logo = logo.resize((BODY, BODY), Image.LANCZOS)

    # Clip the logo to a rounded rect (removes the opaque/black corners).
    mask = rounded_mask(BODY, RADIUS)
    logo.putalpha(mask)

    canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))

    # Soft drop shadow from the body silhouette.
    shadow = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    shadow_body = Image.new("RGBA", (BODY, BODY), (0, 0, 0, SHADOW_ALPHA))
    shadow_body.putalpha(mask.point(lambda a: a * SHADOW_ALPHA // 255))
    shadow.paste(shadow_body, (MARGIN, MARGIN + SHADOW_OFFSET), shadow_body)
    shadow = shadow.filter(ImageFilter.GaussianBlur(SHADOW_BLUR))

    canvas = Image.alpha_composite(canvas, shadow)
    canvas.paste(logo, (MARGIN, MARGIN), logo)
    return canvas


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)
    src = Path(sys.argv[1])
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("Resources/AppIcon.icns")

    master = build_master(src)

    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "AppIcon.iconset"
        iconset.mkdir()
        cache = {}
        for name, size in ICONSET_FILES:
            if size not in cache:
                cache[size] = master.resize((size, size), Image.LANCZOS)
            cache[size].save(iconset / name)
        subprocess.run(
            ["iconutil", "-c", "icns", str(iconset), "-o", str(out)],
            check=True,
        )
    print(f"==> Wrote {out} ({out.stat().st_size // 1024} KB)")


if __name__ == "__main__":
    main()
