#!/usr/bin/env python3
"""Generate the DMG window background image.

Renders at 2x (1280x800) for a 640x400-point Finder window. Draws brand styling,
an arrow from the app-icon slot toward the Applications slot, and install copy.
The real app icon and Applications symlink are placed on top by Finder; this
image only provides the backdrop and the arrow between the two icon slots.

Usage: /usr/bin/python3 scripts/generate-dmg-background.py [output.png]
"""
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

SCALE = 2
W, H = 640 * SCALE, 400 * SCALE
BRAND = (0, 104, 254)           # #0068FE — sampled from the logo
INK = (28, 36, 52)
SUBTLE = (122, 134, 154)

# Icon slot centers (points), matching make-dmg.sh AppleScript positions.
APP_SLOT = (165, 200)
APPS_SLOT = (475, 200)


def load_font(bold, size):
    candidates = (
        [
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        ]
        if bold
        else [
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ]
    )
    for path in candidates:
        try:
            return ImageFont.truetype(path, size)
        except OSError:
            continue
    return ImageFont.load_default()


def vertical_gradient(top, bottom):
    base = Image.new("RGB", (1, H))
    for y in range(H):
        t = y / (H - 1)
        base.putpixel(
            (0, y),
            tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3)),
        )
    return base.resize((W, H))


def centered_text(draw, cx, y, text, font, fill):
    bbox = draw.textbbox((0, 0), text, font=font)
    w = bbox[2] - bbox[0]
    draw.text((cx - w / 2, y), text, font=font, fill=fill)


def draw_arrow(draw):
    # Horizontal arrow between the icon slots, at icon vertical center.
    y = APP_SLOT[1] * SCALE
    x0 = (APP_SLOT[0] + 80) * SCALE
    x1 = (APPS_SLOT[0] - 80) * SCALE
    shaft_h = 7 * SCALE
    head_w = 22 * SCALE
    head_h = 26 * SCALE
    tip = x1
    shaft_end = x1 - head_w

    draw.rounded_rectangle(
        [x0, y - shaft_h // 2, shaft_end, y + shaft_h // 2],
        radius=shaft_h // 2,
        fill=BRAND,
    )
    draw.polygon(
        [(shaft_end - 2, y - head_h // 2), (shaft_end - 2, y + head_h // 2), (tip, y)],
        fill=BRAND,
    )


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("Resources/dmg-background.png")

    img = vertical_gradient((255, 255, 255), (238, 243, 251)).convert("RGBA")
    draw = ImageDraw.Draw(img)

    # Faint brand wash behind the title.
    glow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    gdraw = ImageDraw.Draw(glow)
    gdraw.ellipse(
        [W // 2 - 360, -260, W // 2 + 360, 180],
        fill=BRAND + (26,),
    )
    img = Image.alpha_composite(img, glow)
    draw = ImageDraw.Draw(img)

    title_font = load_font(True, 38 * SCALE)
    sub_font = load_font(False, 15 * SCALE)
    hint_font = load_font(False, 12 * SCALE)

    centered_text(draw, W // 2, 40 * SCALE, "Install TabTypist", title_font, INK)
    centered_text(
        draw,
        W // 2,
        86 * SCALE,
        "Drag the app onto the Applications folder",
        sub_font,
        SUBTLE,
    )

    draw_arrow(draw)

    centered_text(
        draw,
        W // 2,
        330 * SCALE,
        "Then launch TabTypist from Applications or Spotlight",
        hint_font,
        SUBTLE,
    )

    img.convert("RGB").save(out, dpi=(144, 144))
    print(f"==> Wrote {out} ({out.stat().st_size // 1024} KB, {W}x{H})")


if __name__ == "__main__":
    main()
