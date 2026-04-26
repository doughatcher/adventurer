#!/usr/bin/env python3
"""install-steam-art.py — generate placeholder library art for the Adventurer
non-Steam shortcut and copy it into Steam's per-shortcut grid art folder.

Idempotent. Safe to re-run. Generates intentional-looking placeholder art
into ``assets/steam/`` and then sideloads it into
``~/.steam/steam/userdata/<id>/config/grid/`` named with the appid Steam
itself uses for the shortcut.

This is the local-sideload analog of a Steamworks depot upload — the
files have the same dimensions Steamworks accepts, so when this graduates
to a real Steamworks early-access release the source files in
``assets/steam/`` are ready to upload as-is.

Usage:
    # Default — install for the current Steam shortcut "Adventurer" pointing
    # at ~/bin/adventurer-launch.sh
    python3 scripts/install-steam-art.py

    # Different name (e.g. legacy "Adventure Log") or different exe
    python3 scripts/install-steam-art.py --name "Adventure Log"
    python3 scripts/install-steam-art.py --exec /usr/local/bin/adventurer

    # Just regenerate the source PNGs in assets/steam/ without touching Steam
    python3 scripts/install-steam-art.py --no-install

Requires Pillow (pre-installed system-wide on bazzite-desktop).
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
import zlib
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFilter, ImageFont
except ImportError:
    sys.exit("FATAL: Pillow is required. Install with: pip install --user Pillow")


REPO_ROOT = Path(__file__).resolve().parent.parent
ART_DIR   = REPO_ROOT / "assets" / "steam"

# Steam grid art conventions for non-Steam (sideloaded) games:
#   {appid}p.png       — vertical "library_capsule" (600 × 900)
#   {appid}_hero.png   — wide hero banner (3840 × 1240)
#   {appid}_logo.png   — transparent logo overlay (1280 × 720)
#   {appid}.png        — header capsule (920 × 430)
#
# These are the same dimensions Steamworks accepts at depot upload time,
# so the same source PNGs become the production upload payload later.
SPECS = [
    ("library_capsule.png", (600, 900),   "p.png",     "vertical"),
    ("library_hero.png",    (3840, 1240), "_hero.png", "horizontal"),
    ("library_logo.png",    (1280, 720),  "_logo.png", "logo"),
    ("header_capsule.png",  (920, 430),   ".png",      "horizontal"),
]

# ── palette ── matches assets/client/style.css accents
BG_TOP    = (14, 17, 22)      # #0e1116 — dark navy
BG_BOTTOM = (26, 34, 48)      # #1a2230 — slightly lifted
ACCENT    = (99, 168, 232)    # #63a8e8 — link blue from the UI
GOLD      = (220, 178, 100)   # #dcb264 — warm parchment gold
DIM       = (180, 195, 215)   # for subtitles


# ────────────────────────────── font picking ──────────────────────────────
def find_font(family_keyword: str, weight_keyword: str = "Bold") -> str:
    """Walk well-known font dirs for a TTF/OTF whose path matches both
    keywords (case-insensitive). Falls back to DejaVu Sans Bold."""
    needles = [family_keyword.lower(), weight_keyword.lower()]
    roots = ["/usr/share/fonts", "/usr/local/share/fonts",
             os.path.expanduser("~/.local/share/fonts")]
    candidates: list[str] = []
    for root in roots:
        for dirpath, _dirs, files in os.walk(root):
            for f in files:
                if not f.lower().endswith((".ttf", ".otf", ".ttc")):
                    continue
                low = f.lower()
                if all(n in low for n in needles):
                    candidates.append(os.path.join(dirpath, f))
    candidates.sort()
    return candidates[0] if candidates else "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf"


SERIF_BOLD = find_font("notoserif", "")            # variable-weight serif
SANS_BOLD  = find_font("montserrat", "bold")       # geometric sans for tagline


# ─────────────────────── art primitives ───────────────────────
def gradient_bg(size: tuple[int, int]) -> Image.Image:
    """Vertical gradient from BG_TOP at top to BG_BOTTOM at bottom."""
    w, h = size
    img = Image.new("RGB", size, BG_TOP)
    px  = img.load()
    for y in range(h):
        t = y / max(h - 1, 1)
        r = int(BG_TOP[0] + (BG_BOTTOM[0] - BG_TOP[0]) * t)
        g = int(BG_TOP[1] + (BG_BOTTOM[1] - BG_TOP[1]) * t)
        b = int(BG_TOP[2] + (BG_BOTTOM[2] - BG_TOP[2]) * t)
        for x in range(w):
            px[x, y] = (r, g, b)
    return img


def rune_circle(size: int, color: tuple[int, int, int]) -> Image.Image:
    """A faint runic circle motif — concentric rings with hash marks like
    a stylized D20-table calendar. Pure decoration."""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    cx = cy = size // 2
    rgba = (*color, 90)        # mostly transparent
    rgba_dim = (*color, 50)
    # Outer ring
    for r, alpha in ((size // 2 - 4, 90), (size // 2 - 28, 50),
                     (size // 2 - 56, 35)):
        if r > 0:
            d.ellipse([cx - r, cy - r, cx + r, cy + r],
                      outline=(*color, alpha), width=2)
    # 20 hash marks — one for every face on a d20
    import math
    R = size // 2 - 16
    for i in range(20):
        a = (i / 20) * 2 * math.pi
        x1 = cx + math.cos(a) * (R - 8)
        y1 = cy + math.sin(a) * (R - 8)
        x2 = cx + math.cos(a) * (R + 4)
        y2 = cy + math.sin(a) * (R + 4)
        d.line([(x1, y1), (x2, y2)], fill=(*color, 110), width=2)
    # blur for a soft glow
    return img.filter(ImageFilter.GaussianBlur(radius=1))


def draw_title(img: Image.Image, text: str, font_path: str,
               size_px: int, fill=(245, 245, 250),
               glow_color=ACCENT, glow_radius=10,
               anchor="mm", xy=None,
               letter_spacing: float = 0.04) -> None:
    """Draw a glowing title onto img. letter_spacing is fraction of em."""
    if xy is None:
        xy = (img.width // 2, img.height // 2)

    font = ImageFont.truetype(font_path, size_px)
    # Manual letter spacing — PIL doesn't support tracking natively.
    # Render each character separately with a computed x offset.
    char_widths = []
    em = font.getbbox("M")[2]
    extra = int(em * letter_spacing)
    for ch in text:
        bbox = font.getbbox(ch)
        char_widths.append(bbox[2] - bbox[0])
    total = sum(char_widths) + extra * (len(text) - 1)
    # Vertical metrics from a big-cap glyph
    asc_bbox = font.getbbox("Mg")
    height   = asc_bbox[3] - asc_bbox[1]

    if anchor == "mm":
        start_x = xy[0] - total // 2
        baseline_y = xy[1] - height // 2
    elif anchor == "lm":
        start_x = xy[0]
        baseline_y = xy[1] - height // 2
    else:
        start_x, baseline_y = xy

    # Glow layer
    if glow_radius > 0:
        glow_img = Image.new("RGBA", img.size, (0, 0, 0, 0))
        gd = ImageDraw.Draw(glow_img)
        x = start_x
        for ch, w in zip(text, char_widths):
            gd.text((x, baseline_y), ch, font=font, fill=(*glow_color, 200))
            x += w + extra
        glow_img = glow_img.filter(ImageFilter.GaussianBlur(radius=glow_radius))
        img.alpha_composite(glow_img) if img.mode == "RGBA" else img.paste(
            glow_img, (0, 0), glow_img)

    d = ImageDraw.Draw(img)
    x = start_x
    for ch, w in zip(text, char_widths):
        d.text((x, baseline_y), ch, font=font, fill=fill)
        x += w + extra


def make_capsule_vertical() -> Image.Image:
    """library_capsule — 600 × 900 vertical."""
    img = gradient_bg((600, 900)).convert("RGBA")
    # Decorative rune circle, top-half
    rune = rune_circle(440, ACCENT)
    img.alpha_composite(rune, ((img.width - rune.width) // 2, 110))
    # Title
    draw_title(img, "ADVENTURER", SERIF_BOLD, 64,
               fill=(245, 245, 250), glow_color=ACCENT, glow_radius=14,
               xy=(img.width // 2, 580), letter_spacing=0.10)
    # Subtitle
    sub_font = ImageFont.truetype(SANS_BOLD, 22)
    d = ImageDraw.Draw(img)
    sub = "Live D&D session companion"
    bbox = d.textbbox((0, 0), sub, font=sub_font)
    d.text(((img.width - bbox[2]) // 2, 660), sub, font=sub_font, fill=DIM)
    # Footer accent line
    d.line([(120, 760), (480, 760)], fill=(*GOLD, 200), width=2)
    foot_font = ImageFont.truetype(SANS_BOLD, 18)
    foot = "DM toolkit · transcript · LLM state"
    bbox = d.textbbox((0, 0), foot, font=foot_font)
    d.text(((img.width - bbox[2]) // 2, 780), foot, font=foot_font, fill=GOLD)
    return img


def make_hero() -> Image.Image:
    """library_hero — 3840 × 1240 wide banner."""
    img = gradient_bg((3840, 1240)).convert("RGBA")
    # Big rune circle, far-left, partly off-canvas (very common Steam hero comp)
    rune = rune_circle(1600, ACCENT)
    img.alpha_composite(rune, (-200, (img.height - rune.height) // 2))
    rune_r = rune_circle(900, GOLD)
    img.alpha_composite(rune_r, (img.width - 700, 100))
    # Title — shifted right of center to leave room for the Steam-rendered logo
    # overlay (which lives in library_logo.png and floats over the hero in
    # Steam's library detail view). For sideload art we still draw it here
    # because we don't know if the user has logo overlay enabled.
    draw_title(img, "ADVENTURER", SERIF_BOLD, 220,
               fill=(245, 245, 250), glow_color=ACCENT, glow_radius=30,
               xy=(img.width // 2, img.height // 2 - 40), letter_spacing=0.10)
    # Tagline
    sub_font = ImageFont.truetype(SANS_BOLD, 60)
    d = ImageDraw.Draw(img)
    sub = "Live D&D session companion · transcript-driven"
    bbox = d.textbbox((0, 0), sub, font=sub_font)
    d.text(((img.width - bbox[2]) // 2, img.height // 2 + 130), sub,
           font=sub_font, fill=DIM)
    return img


def make_logo() -> Image.Image:
    """library_logo — 1280 × 720 transparent overlay."""
    img = Image.new("RGBA", (1280, 720), (0, 0, 0, 0))
    draw_title(img, "ADVENTURER", SERIF_BOLD, 150,
               fill=(245, 245, 250), glow_color=ACCENT, glow_radius=18,
               xy=(img.width // 2, img.height // 2), letter_spacing=0.10)
    return img


def make_header() -> Image.Image:
    """header_capsule — 920 × 430 horizontal."""
    img = gradient_bg((920, 430)).convert("RGBA")
    rune = rune_circle(420, ACCENT)
    img.alpha_composite(rune, (-60, 5))
    # Title slightly off-center to right so rune motif sits left
    draw_title(img, "ADVENTURER", SERIF_BOLD, 64,
               fill=(245, 245, 250), glow_color=ACCENT, glow_radius=10,
               xy=(530, 175), letter_spacing=0.08)
    sub_font = ImageFont.truetype(SANS_BOLD, 22)
    d = ImageDraw.Draw(img)
    d.text((300, 245), "Live D&D companion", font=sub_font, fill=DIM)
    d.line([(300, 290), (820, 290)], fill=(*GOLD, 180), width=2)
    foot_font = ImageFont.truetype(SANS_BOLD, 16)
    d.text((300, 305), "DM toolkit · transcript · LLM state",
           font=foot_font, fill=GOLD)
    return img


GENERATORS = {
    "library_capsule.png": make_capsule_vertical,
    "library_hero.png":    make_hero,
    "library_logo.png":    make_logo,
    "header_capsule.png":  make_header,
}


# ─────────────────────── steam wiring ───────────────────────
def shortcut_appid_unsigned(exe: str, name: str) -> int:
    """The unsigned-32-bit appid Steam uses for grid-art FILENAMES.

    Steam stores the APPID in the shortcut entry as a SIGNED 32-bit int
    (high bit set), but writes grid art files using the UNSIGNED 32-bit
    representation of that same value. add-to-steam.py returns the signed
    form for shortcuts.vdf; we need the unsigned form here.
    """
    appid_signed = zlib.crc32((exe + name).encode()) | 0x80000000
    # crc32 is already unsigned in Python 3, but be explicit:
    return appid_signed & 0xFFFFFFFF


def find_grid_dir() -> Path | None:
    """Locate the per-user Steam grid art directory."""
    candidates = [
        Path.home() / ".steam" / "steam" / "userdata",
        Path.home() / ".local/share/Steam/userdata",
    ]
    for root in candidates:
        if root.is_dir():
            users = [d for d in root.iterdir() if d.is_dir() and d.name.isdigit()]
            if users:
                # Pick the most-recently-modified user
                users.sort(key=lambda d: d.stat().st_mtime, reverse=True)
                grid = users[0] / "config" / "grid"
                grid.mkdir(parents=True, exist_ok=True)
                return grid
    return None


# ─────────────────────────── main ───────────────────────────
def main() -> int:
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--exec", dest="exe",
                   default=os.path.expanduser("~/bin/adventurer-launch.sh"),
                   help="exe path used by the Steam shortcut (must match add-to-steam.py)")
    p.add_argument("--name", default="Adventurer",
                   help="Steam shortcut display name (must match add-to-steam.py)")
    p.add_argument("--no-install", action="store_true",
                   help="generate source PNGs only; don't copy into Steam grid dir")
    args = p.parse_args()

    ART_DIR.mkdir(parents=True, exist_ok=True)

    # 1. Generate the four source PNGs into assets/steam/
    print(f"==> Generating source art into {ART_DIR}")
    for filename, size, _suffix, _kind in SPECS:
        out_path = ART_DIR / filename
        gen = GENERATORS[filename]
        img = gen()
        if img.size != size:
            img = img.resize(size, Image.LANCZOS)
        img.save(out_path, "PNG", optimize=True)
        print(f"    ✓ {filename}  ({size[0]}×{size[1]})  {out_path.stat().st_size//1024} KB")

    if args.no_install:
        return 0

    # 2. Compute Steam appid + locate grid dir
    appid = shortcut_appid_unsigned(args.exe, args.name)
    print(f"\n==> Steam appid for ({args.name!r}, {args.exe!r}) = {appid}")
    grid = find_grid_dir()
    if not grid:
        print("WARN: couldn't find Steam userdata/grid dir — art generated, "
              "but not installed. Re-run after launching Steam at least once.")
        return 0
    print(f"    grid dir: {grid}")

    # 3. Copy each generated PNG to the Steam grid filename for this appid
    print(f"\n==> Installing art for shortcut appid {appid}")
    for filename, _size, suffix, _kind in SPECS:
        src = ART_DIR / filename
        dst = grid / f"{appid}{suffix}"
        shutil.copy2(src, dst)
        print(f"    ✓ {filename:25s} → {dst.name}")

    print("\nDone. Restart Steam (or refresh the library view) to see the art.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
