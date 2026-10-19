"""Dev-only: generate CleanSweep app icons (PNG + multi-size ICO) with Pillow.

Run: python tools/make-icons.py
Outputs into src-tauri/icons/: 32x32.png, 128x128.png, 128x128@2x.png, icon.png, icon.ico
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ICONS = ROOT / "src-tauri" / "icons"
ICONS.mkdir(parents=True, exist_ok=True)

SIZE = 1024
BG = (17, 20, 27, 255)       # --base
PANEL = (26, 30, 39, 255)    # --panel
AMBER = (232, 163, 61, 255)  # --amber
SAFE = (63, 185, 80, 255)    # --safe
INK = (221, 226, 234, 255)   # --ink

img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
d = ImageDraw.Draw(img)

margin = 52
d.rounded_rectangle(
    [margin, margin, SIZE - margin, SIZE - margin],
    radius=214,
    fill=BG,
    outline=AMBER,
    width=26,
)
d.rounded_rectangle(
    [margin + 68, margin + 68, SIZE - margin - 68, SIZE - margin - 68],
    radius=160,
    fill=PANEL,
)

font = None
for candidate in (
    r"C:\Windows\Fonts\seguisb.ttf",
    r"C:\Windows\Fonts\segoeuib.ttf",
    r"C:\Windows\Fonts\arialbd.ttf",
):
    try:
        font = ImageFont.truetype(candidate, 400)
        break
    except OSError:
        continue
if font is None:  # pragma: no cover - minimal fallback
    font = ImageFont.load_default()

text = "CS"
bbox = d.textbbox((0, 0), text, font=font)
tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
d.text(
    ((SIZE - tw) / 2 - bbox[0], (SIZE - th) / 2 - bbox[1] - 26),
    text,
    font=font,
    fill=AMBER,
)

# 底部绿色状态条，呼应“清理/安全”主题
bar_w, bar_h = 300, 30
d.rounded_rectangle(
    [(SIZE - bar_w) / 2, SIZE * 0.745, (SIZE + bar_w) / 2, SIZE * 0.745 + bar_h],
    radius=bar_h / 2,
    fill=SAFE,
)

img.save(ICONS / "icon.png")
img.resize((256, 256), Image.LANCZOS).save(ICONS / "128x128@2x.png")
img.resize((128, 128), Image.LANCZOS).save(ICONS / "128x128.png")
img.resize((32, 32), Image.LANCZOS).save(ICONS / "32x32.png")
img.save(
    ICONS / "icon.ico",
    format="ICO",
    sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
)
print("icons written to", ICONS)
