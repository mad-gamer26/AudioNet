"""Draws the iPhone app icon (1024x1024, opaque, as App Store Connect
requires): two devices joined by a sound wave, white on AudioNet blue.

    python apps/ios/make-icon.py

writes apps/ios/AudioNet/Assets.xcassets/AppIcon.appiconset/icon-1024.png.
Needs Pillow.
"""
import math, os
from PIL import Image, ImageDraw

S = 1024
SCALE = 4  # draw large, then shrink, for smooth edges
N = S * SCALE
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "AudioNet", "Assets.xcassets", "AppIcon.appiconset", "icon-1024.png")

img = Image.new("RGB", (N, N))
d = ImageDraw.Draw(img)
# Vertical gradient, AudioNet blue (#0B57D0) to a deeper blue.
top, bottom = (0x1A, 0x6B, 0xE6), (0x07, 0x3A, 0x8F)
for y in range(N):
    t = y / (N - 1)
    d.line([(0, y), (N, y)], fill=tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)))

white = (255, 255, 255)
u = N / 1024


def rounded(x0, y0, x1, y1, r, **kw):
    d.rounded_rectangle([x0 * u, y0 * u, x1 * u, y1 * u], radius=r * u, **kw)


# Two devices: a phone on the left, a laptop screen on the right.
rounded(150, 330, 330, 690, 34, outline=white, width=round(30 * u))
rounded(684, 420, 904, 590, 24, outline=white, width=round(30 * u))
rounded(650, 612, 938, 644, 16, fill=white)

# The sound wave between them: bars following a sine envelope.
bars = 7
x0, x1 = 390, 640
for i in range(bars):
    x = x0 + (x1 - x0) * i / (bars - 1)
    h = 60 + 190 * math.sin(math.pi * (i + 0.5) / bars)
    rounded(x - 14, 512 - h / 2, x + 14, 512 + h / 2, 14, fill=white)

os.makedirs(os.path.dirname(OUT), exist_ok=True)
img.resize((S, S), Image.LANCZOS).save(OUT, optimize=True)
print(f"wrote {OUT}")
