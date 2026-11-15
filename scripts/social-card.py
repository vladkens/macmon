#!/usr/bin/env python3
"""GitHub social preview card: a synthetic TUI frame next to the name and tagline.

Usage: scripts/social-card.py

Dumps the frame of the ignored test dump_card_screen (src/app/tui/mod.rs), draws it as SVG and
renders out/social-card.png, 2560x1280 (1280x640 at 2x), with a headless Chromium: Helium or
Chrome, or the browser in $BROWSER. Needs the JetBrains Mono font. Upload the PNG by hand:
Settings > General > Social preview > Edit.
"""

import json
import os
import subprocess
import sys
import tempfile
from html import escape
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "out"
BROWSERS = [
    "/Applications/Helium.app/Contents/MacOS/Helium",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
]

# Catppuccin Macchiato
PAL = {
    "Reset": "#cad3f5", "DarkGray": "#5b6078", "Gray": "#a5adcb", "Green": "#a6da95",
    "Yellow": "#eed49f", "Red": "#ed8796", "Cyan": "#8bd5ca", "Blue": "#8aadf4",
    "Magenta": "#f5bde6", "White": "#b8c0e0", "Black": "#494d64",
}  # fmt: skip
TERM_BG = "#24273a"
BOLD, DIM, REVERSED = 1, 2, 64
BLOCKS = {"▁": 1, "▂": 2, "▃": 3, "▄": 4, "▅": 5, "▆": 6, "▇": 7, "█": 8}

CW, CH = 8.4, 16.8  # cell size
FS = 14.0  # font size: JetBrains Mono advance is 0.6 em
TX, TY = 400, 104  # terminal content origin
BAR = 28  # window title bar

HEAD = """<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="640" viewBox="0 0 1280 640">
<defs>
<linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
  <stop offset="0" stop-color="#1e2030"/><stop offset="1" stop-color="#181926"/>
</linearGradient>
<radialGradient id="glow" cx="0.78" cy="0.55" r="0.6">
  <stop offset="0" stop-color="#a6da95" stop-opacity="0.16"/>
  <stop offset="1" stop-color="#a6da95" stop-opacity="0"/>
</radialGradient>
<filter id="shadow" x="-10%" y="-10%" width="120%" height="130%">
  <feDropShadow dx="0" dy="10" stdDeviation="14" flood-color="#000" flood-opacity="0.5"/>
</filter>
</defs>
<rect width="1280" height="640" fill="url(#bg)"/><rect width="1280" height="640" fill="url(#glow)"/>"""

TAGLINE = ["Sudoless performance", "monitor for Apple", "Silicon Macs"]
FEATURES = [
    ("#a6da95", "CPU, GPU, ANE power"),
    ("#eed49f", "Processes by power"),
    ("#8bd5ca", "JSON and Prometheus"),
    ("#ed8796", "No root needed"),
]


def dump_screen(path):
    env = {**os.environ, "CARD_OUT": str(path)}
    cmd = ["cargo", "test", "-q", "--bin", "macmon", "dump_card_screen", "--", "--ignored"]
    subprocess.run(cmd, cwd=ROOT, env=env, check=True, stdout=subprocess.DEVNULL)
    return json.loads(path.read_text())


def card_svg(screen):
    w, h, cells = screen["w"], screen["h"], screen["cells"]
    out = [HEAD, '<g font-family="JetBrains Mono, monospace">']
    add = out.append

    # left: wordmark, tagline, features
    add('<text x="80" y="200" font-size="64" font-weight="800" fill="#cad3f5">macmon</text>')
    add('<rect x="82" y="222" width="72" height="6" rx="3" fill="#a6da95"/>')
    for i, line in enumerate(TAGLINE):
        add(f'<text x="80" y="{282 + i * 30}" font-size="22" fill="#b8c0e0">{line}</text>')
    for i, (dot, label) in enumerate(FEATURES):
        y = 410 + i * 34
        add(f'<circle cx="88" cy="{y - 6}" r="5" fill="{dot}"/>')
        add(f'<text x="104" y="{y}" font-size="17" fill="#a5adcb">{label}</text>')
    add("</g>")

    # terminal window
    wx, wy = TX - 12, TY - BAR - 8
    ww, wh = w * CW + 24, h * CH + BAR + 20
    add(
        f'<g filter="url(#shadow)"><rect x="{wx}" y="{wy}" width="{ww}" height="{wh}" rx="12" '
        f'fill="{TERM_BG}" stroke="#363a4f"/></g>'
    )
    for i, color in enumerate(["#ed8796", "#eed49f", "#a6da95"]):
        add(f'<circle cx="{wx + 20 + i * 20}" cy="{wy + 18}" r="6" fill="{color}"/>')

    # cell edges snapped to whole pixels, so neighbours share an edge without anti-aliased seams
    def cols(x):
        return round(TX + x * CW), round(TX + (x + 1) * CW)

    bars = {}  # (column, color) -> [(top, bottom)], one rect per vertical run
    add(f'<g font-family="JetBrains Mono, monospace" font-size="{FS}" style="white-space:pre">')
    for y in range(h):
        y0, y1 = round(TY + y * CH), round(TY + (y + 1) * CH)
        runs = []  # (x, style, text)
        for x in range(w):
            sym, fg, bg, mods = cells[y * w + x]
            fgc, bgc = PAL.get(fg, PAL["Reset"]), (None if bg == "Reset" else PAL.get(bg))
            if mods & REVERSED:
                fgc, bgc = (TERM_BG if bg == "Reset" else PAL.get(bg)), fgc
            x0, x1 = cols(x)
            if bgc:
                add(
                    f'<rect x="{x0}" y="{y0}" width="{x1 - x0}" height="{y1 - y0}" fill="{bgc}" '
                    'shape-rendering="crispEdges"/>'
                )
            if sym in BLOCKS:
                top = round(TY + (y + 1) * CH - CH * BLOCKS[sym] / 8)
                bars.setdefault((x, fgc), []).append((top, y1))
                runs.append((x, None, ""))
                continue
            style = (fgc, bool(mods & BOLD), bool(mods & DIM))
            if runs and runs[-1][1] == style:
                runs[-1] = (runs[-1][0], style, runs[-1][2] + sym)
            else:
                runs.append((x, style, sym))
        for x, style, text in runs:
            if not text.strip():
                continue
            fill, bold, dim = style
            attrs = f'fill="{fill}"' + (' font-weight="700"' if bold else "")
            attrs += ' opacity="0.6"' if dim else ""
            text = escape(text).replace(" ", "\u00a0")
            add(f'<text x="{TX + x * CW:.1f}" y="{TY + y * CH + CH * 0.76:.1f}" {attrs}>{text}</text>')

    for (x, fill), segs in bars.items():
        x0, x1 = cols(x)
        segs.sort()
        top, bottom = segs[0]
        for t, b in segs[1:] + [(None, None)]:
            if t is not None and t <= bottom:
                bottom = max(bottom, b)
                continue
            add(
                f'<rect x="{x0}" y="{top}" width="{x1 - x0}" height="{bottom - top}" fill="{fill}" '
                'shape-rendering="crispEdges"/>'
            )
            top, bottom = t, b
    add("</g></svg>")
    return "\n".join(out)


def render_png(svg, png):
    browser = os.environ.get("BROWSER") or next((x for x in BROWSERS if Path(x).exists()), None)
    if not browser:
        sys.exit("No Helium or Chrome found, set BROWSER")
    with tempfile.TemporaryDirectory() as tmp:
        page = Path(tmp) / "card.html"
        style = "html,body{margin:0;background:#181926}svg{display:block}"
        page.write_text(f'<!doctype html><meta charset="utf-8"><style>{style}</style>\n{svg}')
        args = [
            browser, "--headless=new", f"--user-data-dir={tmp}/profile", "--hide-scrollbars",
            "--force-device-scale-factor=2", "--window-size=1280,640", f"--screenshot={png}",
            page.as_uri(),
        ]  # fmt: skip
        subprocess.run(args, check=True, capture_output=True)


def main():
    OUT.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        screen = dump_screen(Path(tmp) / "screen.json")
    svg = card_svg(screen)
    (OUT / "social-card.svg").write_text(svg)
    render_png(svg, OUT / "social-card.png")
    print(OUT / "social-card.png")


if __name__ == "__main__":
    main()
