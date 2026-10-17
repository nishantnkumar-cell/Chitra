"""Draws the Chtra "lotus aperture" icon and writes assets/chtra.png,
assets/chtra.ico and the Linux hicolor icons in packaging/linux/icons. Run
from the repository root: python3 tools/make_icon.py (needs Pillow)."""

import math
import os
from PIL import Image, ImageChops, ImageDraw, ImageFilter

APP_ID = "io.github.nishantnkumar_cell.Chtra"
N = 1024  # drawing size; every output is downsampled from this
C = N / 2


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def vertical_gradient(top, bottom):
    g = Image.new("RGBA", (N, N))
    d = ImageDraw.Draw(g)
    for y in range(N):
        d.line([(0, y), (N, y)], fill=lerp(top, bottom, y / (N - 1)) + (255,))
    return g


def radial_gradient(inner, outer, r0, r1):
    g = Image.new("RGBA", (N, N))
    px = g.load()
    for y in range(N):
        for x in range(N):
            r = math.hypot(x - C, y - C)
            t = min(max((r - r0) / (r1 - r0), 0.0), 1.0)
            px[x, y] = lerp(inner, outer, t) + (255,)
    return g


def squircle_mask(margin, n=5.0):
    m = Image.new("L", (N, N), 0)
    a = C - margin
    pts = []
    for i in range(720):
        t = 2 * math.pi * i / 720
        c, s = math.cos(t), math.sin(t)
        pts.append((C + a * math.copysign(abs(c) ** (2 / n), c),
                    C + a * math.copysign(abs(s) ** (2 / n), s)))
    ImageDraw.Draw(m).polygon(pts, fill=255)
    return m


def petal(angle, length, width, start, shift, steps=200):
    """Pointed lotus petal along `angle`, its axis pushed sideways by
    `shift` so the petals overlap like the blades of an iris."""
    ca, sa = math.cos(angle), math.sin(angle)
    left, right = [], []
    for i in range(steps + 1):
        s = i / steps
        w = width * math.sin(math.pi * s) ** 0.85 * (1 - 0.25 * s)
        along = start + length * s
        for side, out in ((1, left), (-1, right)):
            px, py = along, shift + side * w
            out.append((C + px * ca - py * sa, C + px * sa + py * ca))
    return left + right[::-1]


def hexagon(r, rot):
    return [(C + r * math.cos(rot + k * math.pi / 3),
             C + r * math.sin(rot + k * math.pi / 3)) for k in range(6)]


def draw():
    img = Image.new("RGBA", (N, N), (0, 0, 0, 0))

    # Tile: deep indigo to plum squircle.
    bg = vertical_gradient((38, 24, 82), (110, 30, 92))
    img.paste(bg, (0, 0), squircle_mask(40))

    petals = 6
    # Back ring: wide, soft petals between the front ones (the lotus).
    back = Image.new("L", (N, N), 0)
    for k in range(petals):
        a = -math.pi / 2 + (k + 0.5) * 2 * math.pi / petals
        ImageDraw.Draw(back).polygon(petal(a, 330, 92, 70, 0), fill=255)
    back_fill = radial_gradient((255, 214, 140), (236, 96, 150), 60, 420)
    back_fill.putalpha(back.point(lambda v: v * 150 // 255))
    img.alpha_composite(back_fill)

    # Front ring: the aperture blades, each turned the same way.
    front_fill = radial_gradient((255, 196, 84), (233, 50, 120), 40, 400)
    shade = (120, 20, 70)
    for k in range(petals):
        a = -math.pi / 2 + k * 2 * math.pi / petals
        m = Image.new("L", (N, N), 0)
        ImageDraw.Draw(m).polygon(petal(a, 360, 104, 40, 46), fill=255)
        # Thin dark outline so overlapping blades stay separate when small.
        edge = m.filter(ImageFilter.MaxFilter(13))
        outline = Image.new("RGBA", (N, N), (70, 14, 60, 255))
        outline.putalpha(ImageChops.subtract(edge, m).point(lambda v: v * 200 // 255))
        img.alpha_composite(outline)
        layer = front_fill.copy()
        layer.putalpha(m)
        img.alpha_composite(layer)
        # Shade the trailing half of each blade, like an iris.
        half = Image.new("L", (N, N), 0)
        ImageDraw.Draw(half).polygon(petal(a, 360, 52, 40, 46 - 52), fill=255)
        half = ImageChops.multiply(half, m)
        sh = Image.new("RGBA", (N, N), shade + (255,))
        sh.putalpha(half.point(lambda v: v * 70 // 255))
        img.alpha_composite(sh)

    # The aperture opening: a dark hexagon with a lens glint.
    ImageDraw.Draw(img).polygon(hexagon(112, math.pi / 6), fill=(46, 18, 64, 255))
    ImageDraw.Draw(img).polygon(hexagon(96, math.pi / 6), fill=(24, 12, 40, 255))
    glint = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glint)
    gd.ellipse([C - 52, C - 52, C - 14, C - 14], fill=(255, 236, 190, 255))
    gd.ellipse([C + 22, C + 22, C + 38, C + 38], fill=(255, 236, 190, 150))
    img.alpha_composite(glint)
    return img


def main():
    big = draw()
    big.resize((256, 256), Image.LANCZOS).save("assets/chtra.png", optimize=True)
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    frames = [big.resize((s, s), Image.LANCZOS) for s in sizes]
    frames[-1].save("assets/chtra.ico", sizes=[(s, s) for s in sizes],
                    append_images=frames[:-1])
    for s in [16, 24, 32, 48, 64, 128, 256, 512]:
        folder = f"packaging/linux/icons/hicolor/{s}x{s}/apps"
        os.makedirs(folder, exist_ok=True)
        big.resize((s, s), Image.LANCZOS).save(
            f"{folder}/{APP_ID}.png", optimize=True)


if __name__ == "__main__":
    main()
