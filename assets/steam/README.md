# Steam artwork

Drop the following PNGs in here and the local launcher will (eventually)
sideload them as Steam library art for the non-Steam shortcut. Once the
project graduates to a real Steamworks release these same files become the
upload payload — Steamworks accepts the same dimensions, so we don't have
to redo the artwork later.

## Files (drop them here)

| File                  | Dimensions      | Purpose                                                                |
| --------------------- | --------------- | ---------------------------------------------------------------------- |
| `library_capsule.png` | **600 × 900**   | Vertical "library" cover — what shows in your Steam library grid.      |
| `library_hero.png`    | **3840 × 1240** | Wide hero banner that fills the top of the library detail page.        |
| `library_logo.png`    | **1280 × 720**  | Transparent logo (PNG with alpha) overlaid on the hero.                |
| `header_capsule.png`  | **920 × 430**   | Smaller "header" capsule — store page header + recent-games shelf.     |
| `screenshots/*.png`   | 1920 × 1080+    | In-game screenshots used on the store page once we ship to Steam.      |

## Local sideloading (non-Steam game)

Steam stores per-shortcut artwork at:

```
~/.steam/steam/userdata/<userId>/config/grid/
    <appid>p.png   — vertical library_capsule
    <appid>_hero.png
    <appid>_logo.png
    <appid>.png    — header_capsule
```

Where `<appid>` is the **64-bit signed** Steam shortcut id. The
`scripts/add-to-steam.py` helper computes the same CRC32-based id Steam
uses, so artwork installed once stays attached across re-runs of the
script. (TODO: extend `add-to-steam.py` with `--install-art` to copy these
files into place automatically.)

## Future Steamworks integration

When we register on Steamworks, the partner site uses these exact same
file names + dimensions for the published library art, so the only thing
that changes is uploading them through the partner web UI vs the local
sideload. Source assets (e.g. `.svg`, layered `.psd`) should live under
`assets/steam/sources/` so we can iterate on them without losing layers.

## Style guide (placeholder)

- Keep the typography consistent with `assets/client/style.css` — same
  display font (the existing site uses Inter).
- Hero art should leave the right ~25% relatively quiet so the
  Steam-rendered logo overlay doesn't clash.
- Color palette anchors: dark navy `#0e1116`, accent blue `#63a8e8`.
