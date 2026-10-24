"""Regenerates the app icon set in assets/icon/ (run: python3 packaging/make-icons.py)."""
from pathlib import Path

from PIL import Image, ImageDraw

S = 1024
NAVY, AMBER, INK, WHITE = (13, 24, 46), (242, 178, 58), (16, 23, 40), (250, 251, 253)
out = Path(__file__).resolve().parent.parent / "assets" / "icon"
out.mkdir(parents=True, exist_ok=True)

img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
# macOS-style squircle-ish tile with a little margin.
m = 64
d.rounded_rectangle((m, m, S - m, S - m), radius=200, fill=NAVY)

# The caption callout, sitting up-left of the cursor as in the app.
d.rounded_rectangle((150, 250, 690, 470), radius=44, fill=AMBER)
d.rounded_rectangle((210, 318, 360, 360), radius=21, fill=INK)          # "Click"
d.polygon([(405, 318), (435, 339), (405, 360)], fill=INK)               # "›"
d.rounded_rectangle((480, 318, 630, 360), radius=21, fill=INK)          # target
d.rounded_rectangle((210, 392, 520, 420), radius=14, fill=(16, 23, 40, 150))

# Pointer arrow with a dark outline.
tip = (590, 500)
arrow = [(0, 0), (0, 380), (98, 290), (165, 440), (235, 410), (170, 265), (300, 265)]
pts = [(tip[0] + x, tip[1] + y) for x, y in arrow]
d.polygon(pts, fill=WHITE, outline=INK, width=16)

img.save(out / "icon-1024.png")
for size in (512, 256, 128, 64, 32):
    img.resize((size, size), Image.LANCZOS).save(out / f"icon-{size}.png")
img.save(out / "icon.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
img.save(out / "icon.icns")
print("wrote", sorted(p.name for p in out.iterdir()))
