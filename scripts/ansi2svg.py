#!/usr/bin/env python3
"""Turn terminal output with ANSI colours (tmux capture-pane -e) into an SVG screenshot.

    ansi2svg.py IN.ansi OUT.svg [--title TITLE]

Stdlib only. Text is placed cell by cell, so columns stay aligned whatever font the viewer has.
"""
import re
import sys
import unicodedata
from xml.sax.saxutils import escape

CELL = 8.4        # width of one terminal cell
LINE = 18.0       # height of one row
FONT = 14
PAD = 16.0        # space around the terminal
TOP = 36.0        # window title bar
BACKGROUND = '#1a1b26'
FOREGROUND = '#c0caf5'
FONTS = "ui-monospace, 'SF Mono', SFMono-Regular, Menlo, 'JetBrains Mono', 'DejaVu Sans Mono', monospace"
# Tokyo Night, the theme of tmux.conf.
PALETTE = ['#15161e', '#f7768e', '#9ece6a', '#e0af68', '#7aa2f7', '#bb9af7', '#7dcfff', '#a9b1d6',
           '#414868', '#f7768e', '#9ece6a', '#e0af68', '#7aa2f7', '#bb9af7', '#7dcfff', '#c0caf5']
ESCAPE = re.compile(r'\x1b\[([0-9;:?]*)([A-Za-z])|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][A-Za-z0-9]|\x1b.')


def width(char):
    if unicodedata.combining(char) or unicodedata.category(char) in ('Mn', 'Me', 'Cf'):
        return 0
    return 2 if unicodedata.east_asian_width(char) in ('W', 'F') else 1


def colour_256(n):
    if n < 16:
        return PALETTE[n]
    if n < 232:
        n -= 16
        level = (0, 95, 135, 175, 215, 255)
        return '#%02x%02x%02x' % (level[n // 36], level[n // 6 % 6], level[n % 6])
    grey = 8 + 10 * (n - 232)
    return '#%02x%02x%02x' % (grey, grey, grey)


class Style:
    def __init__(self):
        self.reset()

    def reset(self):
        self.fg = self.bg = None
        self.bold = self.dim = self.italic = self.reverse = False

    def key(self):
        return (self.fg, self.bg, self.bold, self.dim, self.italic, self.reverse)

    def apply(self, params):
        codes = [int(p) if p.isdigit() else 0 for p in re.split('[;:]', params)] if params else [0]
        i = 0
        while i < len(codes):
            code = codes[i]
            if code == 0:
                self.reset()
            elif code == 1:
                self.bold = True
            elif code == 2:
                self.dim = True
            elif code == 3:
                self.italic = True
            elif code == 7:
                self.reverse = True
            elif code == 22:
                self.bold = self.dim = False
            elif code == 23:
                self.italic = False
            elif code == 27:
                self.reverse = False
            elif 30 <= code <= 37 or 90 <= code <= 97:
                self.fg = PALETTE[code - 30 if code < 90 else code - 82]
            elif 40 <= code <= 47 or 100 <= code <= 107:
                self.bg = PALETTE[code - 40 if code < 100 else code - 92]
            elif code == 39:
                self.fg = None
            elif code == 49:
                self.bg = None
            elif code in (38, 48) and i + 1 < len(codes):
                if codes[i + 1] == 5 and i + 2 < len(codes):
                    value, i = colour_256(codes[i + 2]), i + 2
                elif codes[i + 1] == 2 and i + 4 < len(codes):
                    value, i = '#%02x%02x%02x' % tuple(codes[i + 2:i + 5]), i + 4
                else:
                    value = None
                if code == 38:
                    self.fg = value
                else:
                    self.bg = value
            i += 1


def runs(line, style):
    """Split one line into (column, text, style key) runs; the style carries over to the next line."""
    out, column, text, start, current = [], 0, '', 0, style.key()
    position = 0
    for match in ESCAPE.finditer(line + '\x1b[999X'):  # a sentinel flushes the last run
        for char in line[position:match.start()]:
            if not char.isprintable():
                continue
            if not text:
                start, current = column, style.key()
            text += char
            column += width(char)
        position = match.end()
        if match.group(2) == 'm':
            if text:
                out.append((start, text, current))
                text = ''
            style.apply(match.group(1))
    if text:
        out.append((start, text, current))
    return out, column


def convert(ansi, title='', cols=None):
    style = Style()
    lines = []
    widest = 0
    for raw in ansi.rstrip('\n').split('\n'):
        parts, column = runs(raw, style)
        lines.append(parts)
        widest = max(widest, column)
    cols = cols or widest
    total_w = 2 * PAD + cols * CELL
    total_h = TOP + PAD + len(lines) * LINE
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{total_w:.0f}" height="{total_h:.0f}" '
           f'viewBox="0 0 {total_w:.0f} {total_h:.0f}" font-family="{escape(FONTS)}" font-size="{FONT}">',
           f'<rect width="100%" height="100%" rx="10" fill="{BACKGROUND}"/>']
    for i, colour in enumerate(('#f7768e', '#e0af68', '#9ece6a')):
        out.append(f'<circle cx="{PAD + 6 + i * 20:.0f}" cy="18" r="6" fill="{colour}"/>')
    if title:
        out.append(f'<text x="{total_w / 2:.1f}" y="23" text-anchor="middle" fill="#565f89">{escape(title)}</text>')
    for row, parts in enumerate(lines):
        top = TOP + row * LINE
        baseline = top + LINE * 0.75
        for column, text, (fg, bg, bold, dim, italic, reverse) in parts:
            cells = sum(width(c) for c in text)
            x = PAD + column * CELL
            fill, back = fg or FOREGROUND, bg
            if reverse:
                fill, back = bg or BACKGROUND, fg or FOREGROUND
            if back:
                out.append(f'<rect x="{x:.1f}" y="{top:.1f}" width="{cells * CELL:.1f}" height="{LINE:.1f}" fill="{back}"/>')
            if not text.strip():
                continue
            attrs = [f'x="{x:.1f}"', f'y="{baseline:.1f}"', f'fill="{fill}"',
                     f'textLength="{cells * CELL:.1f}"', 'lengthAdjust="spacingAndGlyphs"', 'xml:space="preserve"']
            if bold:
                attrs.append('font-weight="bold"')
            if italic:
                attrs.append('font-style="italic"')
            if dim:
                attrs.append('opacity="0.6"')
            out.append(f'<text {" ".join(attrs)}>{escape(text)}</text>')
    out.append('</svg>')
    return '\n'.join(out) + '\n'


def main(argv):
    if len(argv) < 2:
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    title = argv[argv.index('--title') + 1] if '--title' in argv else ''
    with open(argv[0], encoding='utf-8', errors='replace') as source:
        svg = convert(source.read(), title)
    with open(argv[1], 'w', encoding='utf-8') as target:
        target.write(svg)
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
