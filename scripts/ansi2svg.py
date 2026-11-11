#!/usr/bin/env python3
"""A terminal capture (`tmux capture-pane -e -p`) as an SVG: monospace text
on a dark background, with the capture's colours (16, 256 and truecolor,
foreground and background), bold, dim, italic, underline and reverse.

    python3 scripts/ansi2svg.py --cols 110 --rows 30 --title T < in.ansi > out.svg

No dependencies beyond Python 3. Used by scripts/screenshots.
"""

import argparse
import re
import sys
import unicodedata
from html import escape

BG = "#14161b"
FG = "#d4d7dd"
# The 16 ANSI colours (a dark theme), then the 6x6x6 cube and the greys.
BASE16 = [
    "#1d1f24", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
    "#5c6370", "#ff7a85", "#b5e890", "#ffd68a", "#7cc2ff", "#de94f0", "#6fd3df", "#ffffff",
]


def color256(n):
    if n < 16:
        return BASE16[n]
    if n < 232:
        n -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (steps[n // 36], steps[(n // 6) % 6], steps[n % 6])
    g = 8 + (n - 232) * 10
    return "#%02x%02x%02x" % (g, g, g)


def width(ch):
    if unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


class Style:
    __slots__ = ("fg", "bg", "bold", "dim", "italic", "under", "rev")

    def __init__(self):
        self.reset()

    def reset(self):
        self.fg = self.bg = None
        self.bold = self.dim = self.italic = self.under = self.rev = False

    def key(self):
        return (self.fg, self.bg, self.bold, self.dim, self.italic, self.under, self.rev)

    def sgr(self, params):
        ps = [int(p) if p else 0 for p in params.split(";")] if params else [0]
        i = 0
        while i < len(ps):
            p = ps[i]
            if p == 0:
                self.reset()
            elif p == 1:
                self.bold = True
            elif p == 2:
                self.dim = True
            elif p == 3:
                self.italic = True
            elif p == 4:
                self.under = True
            elif p == 7:
                self.rev = True
            elif p == 22:
                self.bold = self.dim = False
            elif p == 23:
                self.italic = False
            elif p == 24:
                self.under = False
            elif p == 27:
                self.rev = False
            elif 30 <= p <= 37:
                self.fg = BASE16[p - 30]
            elif 90 <= p <= 97:
                self.fg = BASE16[p - 90 + 8]
            elif p == 39:
                self.fg = None
            elif 40 <= p <= 47:
                self.bg = BASE16[p - 40]
            elif 100 <= p <= 107:
                self.bg = BASE16[p - 100 + 8]
            elif p == 49:
                self.bg = None
            elif p in (38, 48) and i + 1 < len(ps):
                if ps[i + 1] == 5 and i + 2 < len(ps):
                    c = color256(ps[i + 2])
                    i += 2
                elif ps[i + 1] == 2 and i + 4 < len(ps):
                    c = "#%02x%02x%02x" % tuple(ps[i + 2:i + 5])
                    i += 4
                else:
                    c = None
                    i += 1
                if p == 38:
                    self.fg = c
                else:
                    self.bg = c
            i += 1


CSI = re.compile(r"\x1b\[([0-9;:?]*)([A-Za-z])|\x1b[()][A-Za-z0-9]|\x1b.")


def cells(line, style):
    """The line as (column, char, style key) cells; style carries over lines."""
    out = []
    col = 0
    pos = 0
    for m in CSI.finditer(line + "\x1b[0n"):
        for ch in line[pos:m.start()]:
            w = width(ch)
            if w == 0 and out:
                c, prev, k = out[-1]
                out[-1] = (c, prev + ch, k)
                continue
            out.append((col, ch, style.key()))
            col += w
        pos = m.end()
        if m.group(2) == "m":
            style.sgr(m.group(1).replace(":", ";"))
    return out


def render(text, cols, rows, title):
    cw, lh, fs, pad = 8.4, 18, 14, 14
    top = pad + 26
    w = int(cols * cw + 2 * pad)
    h = int(rows * lh + top + pad)
    style = Style()
    lines = text.replace("\r", "").split("\n")[:rows]
    bgs, texts = [], []
    for y, line in enumerate(lines):
        row = cells(line, style)
        # Runs of the same style, for fewer elements.
        runs = []
        for col, ch, k in row:
            if runs and runs[-1][2] == k and runs[-1][0] + runs[-1][3] == col:
                runs[-1][1] += ch
                runs[-1][3] += sum(width(c) for c in ch)
            else:
                runs.append([col, ch, k, sum(width(c) for c in ch)])
        for col, s, k, n in runs:
            fg, bg, bold, dim, italic, under, rev = k
            if rev:
                fg, bg = (bg or BG), (fg or FG)
            x = pad + col * cw
            ty = top + y * lh
            if bg:
                bgs.append('<rect x="%.1f" y="%d" width="%.1f" height="%d" fill="%s"/>' % (x, ty, n * cw, lh, bg))
            if not s.strip():
                continue
            # One element a word, each character at its own column: the grid
            # holds whatever font draws it, and no renderer's handling of
            # spaces can shift it.
            style_attrs = []
            if fg:
                style_attrs.append('fill="%s"' % fg)
            if bold:
                style_attrs.append('font-weight="bold"')
            if dim:
                style_attrs.append('opacity="0.55"')
            if italic:
                style_attrs.append('font-style="italic"')
            if under:
                style_attrs.append('text-decoration="underline"')
            c = col
            word, xs = "", []
            for ch in s + " ":
                if ch == " ":
                    if word:
                        attrs = ['x="%s"' % " ".join(xs), 'y="%d"' % (ty + 13)] + style_attrs
                        texts.append("<text %s>%s</text>" % (" ".join(attrs), escape(word, quote=False)))
                    word, xs = "", []
                    c += 1
                    continue
                word += ch
                xs.extend(["%.1f" % (pad + c * cw)] * len(ch))
                c += sum(width(x) for x in ch)
    dots = "".join(
        '<circle cx="%d" cy="%d" r="6" fill="%s"/>' % (pad + 8 + i * 20, pad + 6, c)
        for i, c in enumerate(["#ff5f57", "#febc2e", "#28c840"])
    )
    head = (
        '<text x="%d" y="%d" fill="#7f8590" font-family="-apple-system, Segoe UI, Helvetica, Arial, sans-serif" font-size="12" text-anchor="middle">%s</text>'
        % (w // 2, pad + 10, escape(title, quote=False))
        if title
        else ""
    )
    return "\n".join(
        [
            '<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" viewBox="0 0 %d %d" role="img" aria-label="%s">'
            % (w, h, w, h, escape(title or "terminal")),
            '<rect width="100%%" height="100%%" rx="10" fill="%s"/>' % BG,
            dots,
            head,
            '<g font-family="ui-monospace, SFMono-Regular, Menlo, Consolas, \'DejaVu Sans Mono\', monospace" '
            'font-size="%d" fill="%s" xml:space="preserve">' % (fs, FG),
            *bgs,
            *texts,
            "</g>",
            "</svg>",
            "",
        ]
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cols", type=int, default=110)
    ap.add_argument("--rows", type=int, default=30)
    ap.add_argument("--title", default="")
    a = ap.parse_args()
    text = sys.stdin.buffer.read().decode("utf-8", "replace")
    sys.stdout.write(render(text, a.cols, a.rows, a.title))


if __name__ == "__main__":
    main()
