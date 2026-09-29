"""Generate the app icon (assets/icon.png, icon-256.png, icon.ico, icon.icns).
Requires Pillow. The .icns step needs macOS `iconutil`."""
import math, os, shutil, subprocess, tempfile
from PIL import Image, ImageDraw

S = 1024
root = os.path.join(os.path.dirname(__file__), "..", "assets")
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

# Rounded-square background with vertical gradient (macOS style margins).
m, r = 100, 185
grad = Image.new("RGBA", (S, S))
gd = ImageDraw.Draw(grad)
top, bot = (58, 123, 255), (122, 60, 220)
for y in range(S):
    t = y / S
    gd.line([(0, y), (S, y)], fill=tuple(int(top[i] + (bot[i] - top[i]) * t) for i in range(3)) + (255,))
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle([m, m, S - m, S - m], r, fill=255)
img.paste(grad, (0, 0), mask)

d = ImageDraw.Draw(img)
white = (255, 255, 255, 255)
cy = S // 2
# Waveform bars on the left.
heights = [90, 170, 260, 330, 250, 380, 290, 180, 110]
x = 205
for h in heights:
    d.rounded_rectangle([x, cy - h / 2, x + 26, cy + h / 2], 13, fill=white)
    x += 42
# Split: one line forking into three outputs.
fx = x + 10
d.line([(fx, cy), (fx + 60, cy)], fill=white, width=26)
for dy in (-190, 0, 190):
    ex, ey = 800, cy + dy
    d.line([(fx + 50, cy), (ex, ey)], fill=white, width=26, joint="curve")
    d.ellipse([ex - 34, ey - 34, ex + 34, ey + 34], fill=white)
d.ellipse([fx + 50 - 13, cy - 13, fx + 50 + 13, cy + 13], fill=white)

img.save(os.path.join(root, "icon.png"))
img.resize((256, 256), Image.LANCZOS).save(os.path.join(root, "icon-256.png"))
img.save(os.path.join(root, "icon.ico"), sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])

if shutil.which("iconutil"):
    with tempfile.TemporaryDirectory() as tmp:
        iconset = os.path.join(tmp, "icon.iconset")
        os.mkdir(iconset)
        for size in (16, 32, 128, 256, 512):
            img.resize((size, size), Image.LANCZOS).save(os.path.join(iconset, f"icon_{size}x{size}.png"))
            img.resize((size * 2, size * 2), Image.LANCZOS).save(os.path.join(iconset, f"icon_{size}x{size}@2x.png"))
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(root, "icon.icns")], check=True)
print("icons written to", os.path.abspath(root))
