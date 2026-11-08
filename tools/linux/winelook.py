#!/usr/bin/env python3
"""Gives Windows programs (Wine) justOS's look: its colours and typeface,
in every Wine prefix, through Wine's own wine.inf.

Usage: winelook.py <tree>   (the system volume's files; nothing to do
                             without usr/share/wine/wine.inf)

Wine draws windows with the Windows colours in the prefix's registry
(HKCU\\Control Panel\\Colors). wineboot fills a new prefix from wine.inf,
and runs it again on old prefixes when wine.inf changes, so a section of
justOS's values added to what it installs reaches every prefix. The
colours are those of justOS's GTK theme (rootfs/usr/share/themes/justOS):
dark, white text, the orange accent.

The title bar colours are markers too: under Wayland Wine draws its own
frame, and justOS's compositor knows it by them and leaves it out of the
window, which the desktop frames (windows_frame in
system/kernel/src/linux/wayland/mod.rs). Change them together.
"""
import re
import sys
from pathlib import Path

SECTION = "JustOSLook"

COLORS = {
    # The title bars: markers (see above), next to justOS's ink.
    "ActiveTitle": "27 26 30",
    "GradientActiveTitle": "27 26 30",
    "InactiveTitle": "27 26 29",
    "GradientInactiveTitle": "27 26 29",
    "TitleText": "255 255 255",
    "InactiveTitleText": "201 198 206",
    # Surfaces, as justOS's menus and windows.
    "Background": "27 26 31",
    "AppWorkSpace": "27 26 31",
    "Window": "36 35 41",
    "WindowText": "255 255 255",
    "WindowFrame": "14 13 16",
    "ButtonFace": "44 43 49",
    "ButtonAlternateFace": "44 43 49",
    "ButtonText": "255 255 255",
    "ButtonHilight": "78 76 84",
    "ButtonLight": "60 58 66",
    "ButtonShadow": "22 21 25",
    "ButtonDkShadow": "14 13 16",
    "ActiveBorder": "44 43 49",
    "InactiveBorder": "44 43 49",
    "Menu": "44 43 49",
    "MenuBar": "36 35 41",
    "MenuText": "255 255 255",
    "Scrollbar": "36 35 41",
    "GrayText": "148 145 154",
    "InfoWindow": "27 26 31",
    "InfoText": "255 255 255",
    # The accent.
    "Hilight": "255 90 31",
    "HilightText": "255 255 255",
    "MenuHilight": "255 90 31",
    "HotTrackingColor": "255 90 31",
}

# Windows's interface typefaces, as justOS's; its fixed-width ones, as the
# fixed-width typeface the Linux programs have. Wine substitutes only names
# it has no typeface of: those it brings along (Tahoma, System, MS Sans
# Serif, Courier, Fixedsys) stay its own. The interface (menus, dialogs,
# message boxes) is in MS Shell Dlg, which it has not.
TYPEFACE = "Google Sans Flex"
SUBSTITUTED = ["MS Shell Dlg", "MS Shell Dlg 2", "Segoe UI", "Microsoft Sans Serif", "Arial"]
MONO_TYPEFACE = "DejaVu Sans Mono"
MONO_SUBSTITUTED = ["Courier New", "Lucida Console", "Consolas"]


def section() -> list[str]:
    lines = [f"[{SECTION}]"]
    lines += [f'HKCU,"Control Panel\\Colors","{name}",,"{value}"' for name, value in COLORS.items()]
    subst = "Software\\Microsoft\\Windows NT\\CurrentVersion\\FontSubstitutes"
    lines += [f'HKLM,"{subst}","{face}",,"{TYPEFACE}"' for face in SUBSTITUTED]
    lines += [f'HKLM,"{subst}","{face}",,"{MONO_TYPEFACE}"' for face in MONO_SUBSTITUTED]
    # Smooth (greyscale) text, as justOS's own.
    lines += ['HKCU,"Control Panel\\Desktop","FontSmoothing",,"2"',
              'HKCU,"Control Panel\\Desktop","FontSmoothingType",0x10001,1']
    # Notepad writes in Windows's fixed-width typeface (so, justOS's) rather
    # than the old System one, unless the user chose another.
    lines += ['HKCU,"Software\\Microsoft\\Notepad","lfFaceName",0x2,"Consolas"',
              'HKCU,"Software\\Microsoft\\Notepad","iPointSize",0x10003,110']
    # Wine's light visual style would paint over these colours.
    lines += ['HKCU,"Software\\Microsoft\\Windows\\CurrentVersion\\ThemeManager","ThemeActive",,"0"']
    return lines


def patch(text: str) -> str:
    if f"\n[{SECTION}]\n" in text:
        # Patched before: the section (last in the file) is made anew.
        head = text.split(f"\n[{SECTION}]\n")[0]
        return head.rstrip("\n") + "\n\n" + "\n".join(section()) + "\n"
    lines = text.split("\n")
    out = []
    i = 0
    # wineboot installs [DefaultInstall] and its per-platform variants: the
    # section goes last in each one's AddReg list, after Wine's own values.
    header = re.compile(r"^\[DefaultInstall(\.NT|\.ntamd64|\.ntarm64)?\]\s*$")
    in_install = False
    while i < len(lines):
        line = lines[i]
        if line.startswith("["):
            in_install = bool(header.match(line))
        out.append(line)
        i += 1
        if in_install and line.replace(" ", "").startswith("AddReg=\\"):
            # The list goes on while lines end with a backslash.
            while i < len(lines) and lines[i].rstrip().endswith("\\"):
                out.append(lines[i])
                i += 1
            if i < len(lines):
                out.append(lines[i].rstrip() + ",\\")
                out.append(f"    {SECTION}")
                i += 1
    return "\n".join(out).rstrip("\n") + "\n\n" + "\n".join(section()) + "\n"


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    inf = Path(sys.argv[1]) / "usr/share/wine/wine.inf"
    if not inf.exists():
        return 0
    text = inf.read_text(encoding="utf-8", errors="surrogateescape")
    patched = patch(text)
    if patched != text:
        inf.write_text(patched, encoding="utf-8", errors="surrogateescape")
        print(f"winelook: justOS's look for Windows programs in {inf}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
