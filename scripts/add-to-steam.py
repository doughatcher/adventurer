#!/usr/bin/env python3
"""
add-to-steam.py — add (or update) "Adventure Log" as a non-Steam game in the
Steam Linux shortcuts.vdf.

Steam needs to be **fully exited** before running this. If Steam is running it
caches shortcuts.vdf in memory and overwrites your edits on shutdown.

Usage:
    # Default — add the launcher we installed at ~/bin/adventurer-launch.sh
    python3 scripts/add-to-steam.py

    # Custom launcher path / name
    python3 scripts/add-to-steam.py --exec /usr/local/bin/adventurer --name "Adventurer"

After running, start Steam — "Adventure Log" should appear in your library.
"""
import argparse
import os
import shutil
import sys
import time
import zlib
from pathlib import Path

try:
    import vdf
except ImportError:
    sys.exit("missing dep: pip install --user vdf")


def find_shortcuts_vdf() -> Path:
    """Locate the active Steam user's shortcuts.vdf."""
    candidates = []
    for base in [
        Path.home() / ".local/share/Steam/userdata",
        Path.home() / ".steam/steam/userdata",
        Path.home() / ".steam/root/userdata",
    ]:
        if not base.is_dir():
            continue
        for user_dir in base.iterdir():
            if not user_dir.is_dir() or not user_dir.name.isdigit():
                continue
            sc = user_dir / "config/shortcuts.vdf"
            candidates.append(sc)

    if not candidates:
        sys.exit("could not find Steam userdata/<id>/config/ — is Steam installed?")
    if len(candidates) > 1:
        # Prefer the most recently modified userdata dir.
        candidates.sort(key=lambda p: (p.exists(), p.stat().st_mtime if p.exists() else 0), reverse=True)
        print(f"multiple candidates found, using most recent: {candidates[0].parent.parent}")
    return candidates[0]


def shortcut_appid(exe: str, name: str) -> int:
    """Compute Steam's shortcut appid the way Steam itself does."""
    # Steam's algorithm: CRC32 of (exe + name) | 0x80000000, signed 32-bit.
    crc = zlib.crc32((exe + name).encode()) | 0x80000000
    if crc >= 1 << 31:
        crc -= 1 << 32
    return crc


def make_entry(name: str, exe: str, start_dir: str, icon: str | None) -> dict:
    return {
        "appid": shortcut_appid(exe, name),
        "AppName": name,
        "Exe": f'"{exe}"',
        "StartDir": f'"{start_dir}"',
        "icon": icon or "",
        "ShortcutPath": "",
        "LaunchOptions": "",
        "IsHidden": 0,
        "AllowDesktopConfig": 1,
        "AllowOverlay": 1,
        "OpenVR": 0,
        "Devkit": 0,
        "DevkitGameID": "",
        "DevkitOverrideAppID": 0,
        "LastPlayTime": int(time.time()),
        "FlatpakAppID": "",
        "tags": {},
    }


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--name", default="Adventure Log", help="display name in Steam library")
    p.add_argument("--exec", dest="exe", default=str(Path.home() / "bin/adventurer-launch.sh"),
                   help="path to launcher script")
    p.add_argument("--icon", default=None, help="optional icon path")
    p.add_argument("--vdf", default=None, help="explicit path to shortcuts.vdf (auto-detect if omitted)")
    args = p.parse_args()

    exe = os.path.abspath(args.exe)
    if not os.path.isfile(exe):
        sys.exit(f"launcher not found: {exe}")
    start_dir = os.path.dirname(exe)

    vdf_path = Path(args.vdf) if args.vdf else find_shortcuts_vdf()
    print(f"shortcuts.vdf: {vdf_path}")

    # Check Steam isn't running.
    if any(_is_steam_running()):
        sys.exit("⚠  Steam appears to be running. Quit Steam first (System tray → Steam → Exit), then re-run this script.")

    vdf_path.parent.mkdir(parents=True, exist_ok=True)
    if vdf_path.exists():
        with open(vdf_path, "rb") as f:
            data = vdf.binary_load(f)
        # Backup once
        backup = vdf_path.with_suffix(".vdf.bak")
        if not backup.exists():
            shutil.copy2(vdf_path, backup)
            print(f"backup written: {backup}")
    else:
        data = {"shortcuts": {}}

    shortcuts = data.setdefault("shortcuts", {})
    # Remove any existing entry with the same name (idempotent re-runs).
    keep = []
    for k, v in shortcuts.items():
        if (v.get("AppName") or v.get("appname")) != args.name:
            keep.append(v)
    shortcuts.clear()

    new_entry = make_entry(args.name, exe, start_dir, args.icon)
    print(f"adding entry: appid={new_entry['appid']:#010x} name={args.name!r} exe={exe!r}")
    keep.append(new_entry)
    for i, entry in enumerate(keep):
        shortcuts[str(i)] = entry

    with open(vdf_path, "wb") as f:
        vdf.binary_dump(data, f)
    print(f"✓ wrote {len(keep)} shortcut(s)")
    print(f"\nNext: start Steam — '{args.name}' will appear in your library.")
    return 0


def _is_steam_running() -> list[int]:
    """Return PIDs of Steam processes."""
    pids = []
    try:
        for d in Path("/proc").iterdir():
            if not d.name.isdigit():
                continue
            try:
                comm = (d / "comm").read_text().strip()
            except Exception:
                continue
            if comm in ("steam", "steamwebhelper"):
                pids.append(int(d.name))
    except Exception:
        pass
    return pids


if __name__ == "__main__":
    sys.exit(main())
