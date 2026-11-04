#!/usr/bin/env python3
"""Write Timbre's icon files from one drawing.

  assets/icon.svg        full-bleed square, for the README and Claude's list
  assets/icon-macos.svg  the same drawing at macOS proportions (transparent
                         margin, softer shadow), for the Mac app icon
  assets/icon.png        512 px render of icon.svg (Claude's extension icon)
  assets/Timbre.icns     every size macOS asks for, rendered from icon-macos.svg

Needs rsvg-convert (librsvg). Run from the repository root:
python3 -I scripts/make-icons.py
"""
import os
import struct
import subprocess
import sys
import tempfile

PERF_STEP, PERF_R = 20, 9
STAMP = (116, 92, 396, 420)  # left, top, right, bottom


def perforations():
    l, t, r, b = STAMP
    xs = range(l, r + 1, PERF_STEP)
    ys = range(t + PERF_STEP, b, PERF_STEP)
    holes = [(x, t) for x in xs] + [(x, b) for x in xs] + [(l, y) for y in ys] + [(r, y) for y in ys]
    return "\n".join(f'      <circle cx="{x}" cy="{y}" r="{PERF_R}"/>' for x, y in holes)


def drawing(shadow_dy, shadow_blur):
    l, t, r, b = STAMP
    return f"""  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#3730A3"/>
      <stop offset="1" stop-color="#7C3AED"/>
    </linearGradient>
    <mask id="perf" maskUnits="userSpaceOnUse" x="0" y="0" width="512" height="512">
      <rect width="512" height="512" fill="#fff"/>
      <g fill="#000">
{perforations()}
      </g>
    </mask>
    <filter id="shadow" x="-20%" y="-20%" width="140%" height="140%" color-interpolation-filters="sRGB">
      <feGaussianBlur in="SourceAlpha" stdDeviation="{shadow_blur}"/>
      <feOffset dy="{shadow_dy}" result="blur"/>
      <feFlood flood-color="#1E1B4B" flood-opacity="0.45"/>
      <feComposite in2="blur" operator="in"/>
      <feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
  </defs>
  <rect width="512" height="512" rx="114" fill="url(#bg)"/>
  <g filter="url(#shadow)">
    <rect x="{l}" y="{t}" width="{r - l}" height="{b - t}" fill="#FFF8EE" mask="url(#perf)"/>
  </g>
  <rect x="146" y="122" width="220" height="268" rx="6" fill="none" stroke="#3730A3" stroke-width="7"/>
  <rect x="176" y="226" width="160" height="110" rx="14" fill="#F2654D"/>
  <path d="M184 236 256 290 328 236" fill="none" stroke="#FFF8EE" stroke-width="11" stroke-linecap="round" stroke-linejoin="round"/>
  <g fill="none" stroke="#3730A3" stroke-width="7" stroke-linecap="round" opacity="0.85">
    <path d="M252 160q17-13 34 0t34 0 34 0 34 0"/>
    <path d="M252 184q17-13 34 0t34 0 34 0 34 0"/>
    <path d="M252 208q17-13 34 0t34 0 34 0 34 0"/>
  </g>"""


HEADER = (
    '<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="{view}" role="img" '
    'aria-labelledby="title">\n  <title id="title">Timbre</title>\n'
    "  <!-- Timbre icon. Copyright (c) 2026 Alejandro San Martin Lamas — https://github.com/alejandrosml (MIT) -->\n"
)


def full_bleed():
    return HEADER.format(size=512, view="0 0 512 512") + drawing(10, 12) + "\n</svg>\n"


def macos():
    # Apple's grid: the rounded square spans 824 of 1024 px, centred, with a
    # little room below for the shadow macOS expects.
    scale = 824 / 1024
    inset = 512 * (1 - scale) / 2
    body = drawing(10, 12).replace("\n", "\n  ")
    return (
        HEADER.format(size=1024, view="0 0 512 512")
        + '  <defs>\n    <filter id="drop" x="-10%" y="-10%" width="120%" height="125%" color-interpolation-filters="sRGB">\n'
        + '      <feGaussianBlur in="SourceAlpha" stdDeviation="5"/>\n      <feOffset dy="5" result="b"/>\n'
        + '      <feFlood flood-color="#000" flood-opacity="0.3"/>\n      <feComposite in2="b" operator="in"/>\n'
        + '      <feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge>\n    </filter>\n  </defs>\n'
        + f'  <g filter="url(#drop)">\n  <g transform="translate({inset:.2f} {inset - 4:.2f}) scale({scale:.6f})">\n  '
        + body
        + "\n  </g>\n  </g>\n</svg>\n"
    )


def render(svg_path, size, out):
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), "-o", out, svg_path], check=True)


def icns(svg_path, out):
    # Modern icns entries hold PNG data directly.
    entries = [("icp4", 16), ("icp5", 32), ("icp6", 64), ("ic07", 128), ("ic08", 256), ("ic09", 512),
               ("ic10", 1024), ("ic11", 32), ("ic12", 64), ("ic13", 256), ("ic14", 512)]
    chunks = []
    with tempfile.TemporaryDirectory() as tmp:
        for kind, size in entries:
            png = os.path.join(tmp, f"{kind}.png")
            render(svg_path, size, png)
            data = open(png, "rb").read()
            chunks.append(kind.encode() + struct.pack(">I", len(data) + 8) + data)
    body = b"".join(chunks)
    with open(out, "wb") as f:
        f.write(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main():
    os.chdir(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    open("assets/icon.svg", "w").write(full_bleed())
    open("assets/icon-macos.svg", "w").write(macos())
    render("assets/icon.svg", 512, "assets/icon.png")
    icns("assets/icon-macos.svg", "assets/Timbre.icns")
    print("wrote assets/icon.svg, assets/icon-macos.svg, assets/icon.png, assets/Timbre.icns")


if __name__ == "__main__":
    sys.exit(main())
