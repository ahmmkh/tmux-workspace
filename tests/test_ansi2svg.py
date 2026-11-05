"""scripts/ansi2svg.py turns captured terminal output into the README screenshots."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
from ansi2svg import CELL, PAD, animate, convert  # noqa: E402


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


class Animate(unittest.TestCase):
    def setUp(self):
        self.svg = animate([('$ ', 1.0), ('$ ./install.sh', 2.0), ('\x1b[32mdone\x1b[0m', 1.0)], title='first run')

    def test_one_group_per_frame(self):
        for i in range(3):
            self.assertIn(f'class="frame f{i}"', self.svg)

    def test_frames_share_one_loop(self):
        self.assertIn('animation: k0 4.0s step-end infinite', self.svg)
        self.assertIn('@keyframes k1 { 0% { opacity: 0 } 25.000% { opacity: 1 } 75.000% { opacity: 0 } }', self.svg)

    def test_reduced_motion_shows_the_last_frame(self):
        self.assertIn('prefers-reduced-motion', self.svg)
        # Same specificity, later rule wins: g.frame would outrank .f2 and hide every frame.
        self.assertIn('.frame { opacity: 0 } .f2 { opacity: 1 }', self.svg)
        self.assertNotIn('g.frame { opacity', self.svg)

    def test_frames_of_different_sizes_share_the_largest_canvas(self):
        self.assertIn('width="%.0f"' % (2 * PAD + len('$ ./install.sh') * CELL), self.svg)

    def test_animation_is_well_formed_xml(self):
        import xml.dom.minidom
        xml.dom.minidom.parseString(self.svg)


if __name__ == '__main__':
    unittest.main()
