"""scripts/ansi2svg.py turns captured terminal output into the README screenshots."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
from ansi2svg import CELL, PAD, convert  # noqa: E402


class Convert(unittest.TestCase):
    def test_truecolor_and_bold(self):
        svg = convert('\x1b[1;38;2;247;118;142mblocked\x1b[0m')
        self.assertIn('fill="#f7768e"', svg)
        self.assertIn('font-weight="bold"', svg)
        self.assertIn('>blocked</text>', svg)

    def test_escapes_xml(self):
        self.assertIn('&lt;tab&gt; &amp;', convert('<tab> &'))

    def test_wide_chars_take_two_cells(self):
        svg = convert('日\x1b[31mx')
        self.assertIn('x="%.1f"' % (PAD + 2 * CELL), svg)

    def test_256_colour_cube(self):
        self.assertIn('fill="#ff0000"', convert('\x1b[38;5;196mX'))

    def test_background_becomes_a_rectangle(self):
        self.assertIn('<rect x="%.1f"' % PAD, convert('\x1b[48;2;40;52;87m  '))
        self.assertIn('fill="#283457"', convert('\x1b[48;2;40;52;87m  '))

    def test_reverse_video_swaps_colours(self):
        svg = convert('\x1b[7mX')
        self.assertIn('fill="#c0caf5"', svg)  # the default foreground becomes the cell background

    def test_other_escape_sequences_are_dropped(self):
        svg = convert('\x1b]0;title\x07\x1b[?25lhello\x1b(B')
        self.assertIn('>hello</text>', svg)
        self.assertNotIn('\x1b', svg)

    def test_frame_has_a_title_and_fits_the_widest_line(self):
        svg = convert('ab\nabcd', title='dashboard')
        self.assertIn('>dashboard</text>', svg)
        self.assertIn('width="%.0f"' % (2 * PAD + 4 * CELL), svg)

    def test_output_is_well_formed_xml(self):
        import xml.dom.minidom
        xml.dom.minidom.parseString(convert('\x1b[1;31m● blocked\x1b[0m │ <ok>\n日本', title='t & t'))


if __name__ == '__main__':
    unittest.main()
