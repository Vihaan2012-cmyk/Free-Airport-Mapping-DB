"""Draw the AMDB Bridge icon: a runway on a dark display face, with the amber taxi
guidance line and the white ownship chevron of the moving map. Writes
assets/amdb-bridge.ico with 16-256 px images; the small sizes are drawn with less
detail rather than shrunk, so they stay legible in the tray.

    python tools/make_icon.py
"""

import os
from PIL import Image, ImageDraw

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "assets", "amdb-bridge.ico")

FACE = (16, 27, 42, 255)
RIM = (74, 134, 216, 255)
PAVEMENT = (156, 156, 156, 255)
MARKING = (255, 255, 255, 255)
GUIDANCE = (216, 200, 64, 255)
OWNSHIP = (255, 255, 255, 255)


def draw(size):
    ss = 8 if size <= 48 else 4
    s = size * ss
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    u = s / 32.0  # drawing unit: the icon is laid out on a 32-unit grid
    small = size <= 24

    d.rounded_rectangle([0.5 * u, 0.5 * u, 31.5 * u, 31.5 * u], radius=6 * u, fill=FACE, outline=RIM, width=max(1, round((1.6 if small else 1.1) * u)))

    # The runway, running up the face at a slant so it reads as a map, not a road sign.
    rw = [(9.0 * u, 29 * u), (15.5 * u, 29 * u), (23.5 * u, 3 * u), (17.0 * u, 3 * u)]
    d.polygon(rw, fill=PAVEMENT)
    if not small:
        # Centreline dashes along the runway's axis.
        for i in range(5):
            t0 = 0.12 + i * 0.17
            t1 = t0 + 0.08
            p = lambda t: (12.25 * u + (20.25 - 12.25) * u * t, 29 * u - 26 * u * t)
            d.line([p(t0), p(t1)], fill=MARKING, width=max(1, round(0.9 * u)))

    # Amber guidance curving off the runway onto a taxiway.
    guide_w = max(1, round((2.2 if small else 1.5) * u))
    d.arc([2.5 * u, 9 * u, 22.5 * u, 29 * u], start=180, end=290, fill=GUIDANCE, width=guide_w)

    # Ownship chevron, lower right, pointing up the runway's direction.
    cx, cy = (24.5 if not small else 23.5) * u, 22.5 * u
    k = 1.25 if small else 1.0
    chev = [(cx - 4.5 * k * u, cy + 4 * k * u), (cx, cy - 5 * k * u), (cx + 4.5 * k * u, cy + 4 * k * u), (cx, cy + 1.5 * k * u)]
    d.polygon(chev, fill=OWNSHIP, outline=FACE)

    return img.resize((size, size), Image.LANCZOS)


def main():
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    images = [draw(n) for n in sizes]
    images[-1].save(OUT, format="ICO", sizes=[(n, n) for n in sizes], append_images=images[:-1])
    images[-1].save(os.path.join(os.path.dirname(OUT), "amdb-bridge-256.png"))
    print("wrote", OUT)


if __name__ == "__main__":
    main()
