"""The dashboard's pure renderer and key handling. Runs on Python 3.9: no enterContext."""
import dataclasses
import os
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from fixtures import FIXTURE, PREVIEW  # noqa: E402
import dashboard  # noqa: E402
from dashboard import State  # noqa: E402
import workspace_model as model  # noqa: E402

ROWS = model.build_rows(FIXTURE)
GOLDEN = Path(__file__).resolve().parent / 'golden'


def text(lines):
    return [''.join(t for t, _ in line) for line in lines]


class Width(unittest.TestCase):
    def test_cell_width_counts_wide_characters_twice(self):
        self.assertEqual(dashboard.cell_width('ab'), 2)
        self.assertEqual(dashboard.cell_width('日本'), 4)
        self.assertEqual(dashboard.cell_width('🙂'), 2)
        self.assertEqual(dashboard.cell_width('é'), 1)

    def test_fit_truncates_with_an_ellipsis_and_pads(self):
        self.assertEqual(dashboard.fit('terraform', 6), 'terra…')
        self.assertEqual(dashboard.fit('ui', 4), 'ui  ')
        self.assertEqual(dashboard.cell_width(dashboard.fit('日本語ラベル', 5)), 5)


class Render(unittest.TestCase):
    def test_every_line_fills_the_width(self):
        for w, h in ((120, 40), (80, 24), (50, 20), (40, 12), (30, 6)):
            with self.subTest(size=(w, h)):
                lines = dashboard.render(ROWS, State(), w, h, PREVIEW)
                self.assertEqual(len(lines), h)
                self.assertEqual({dashboard.cell_width(line) for line in text(lines)}, {w})

    def test_header_counts(self):
        top = text(dashboard.render(ROWS, State(), 120, 40, []))[0]
        self.assertIn('tmux-workspace', top)
        self.assertIn('1 blocked · 1 done · 1 working · 1 idle', top)
        self.assertIn('6 panes · 4 workspaces', top)

    def test_zero_counts_are_omitted(self):
        rows = [r for r in ROWS if r.status != 'done']
        self.assertNotIn('done', text(dashboard.render(rows, State(), 120, 40, []))[0])

    def test_column_header_and_mockup_cells(self):
        out = '\n'.join(text(dashboard.render(ROWS, State(), 120, 40, PREVIEW)))
        for cell in ('WORKSPACE', 'TAB', 'PANE', 'AGENT', 'STATUS', '1:code', '1:claude', '● blocked', '○ idle', '⇣ parked'):
            self.assertIn(cell, out)

    def test_first_row_is_selected_by_default(self):
        lines = text(dashboard.render(ROWS, State(), 120, 40, []))
        self.assertTrue(lines[2].startswith('│ ▸ api'), lines[2])

    def test_selection_follows_pane_id(self):
        lines = text(dashboard.render(ROWS, State(selected='%5'), 120, 40, []))
        self.assertEqual([line for line in lines if '▸' in line][0].split()[2], 'infra')

    def test_selection_scrolls_into_view(self):
        lines = text(dashboard.render(ROWS, State(selected='%4'), 80, 8, []))
        self.assertTrue(any('▸ docs' in line for line in lines))

    def test_parked_row_shows_its_home_in_the_tab_column(self):
        row = [line for line in text(dashboard.render(ROWS, State(), 120, 40, [])) if '⇣ parked' in line][0]
        self.assertIn('api', row.split('⇣ parked')[1])

    def test_wide_labels_keep_columns_aligned(self):
        rows = [dataclasses.replace(ROWS[0], label='日本語ラベル🙂')] + ROWS[1:]
        lines = text(dashboard.render(rows, State(), 80, 24, []))
        self.assertEqual({dashboard.cell_width(line) for line in lines}, {80})
        # The status mark starts at the same screen column on every row that has one.
        table = [line for line in lines if line.startswith('│') and ('●' in line or '○' in line)]
        self.assertEqual(len(table), 4)
        columns = {dashboard.cell_width(line[:min(i for i in (line.find('●'), line.find('○')) if i >= 0)]) for line in table}
        self.assertEqual(len(columns), 1)

    def test_narrow_hides_agent_column_and_preview(self):
        out = '\n'.join(text(dashboard.render(ROWS, State(), 48, 18, PREVIEW)))
        self.assertNotIn('AGENT', out)
        self.assertNotIn('preview', out)

    def test_tall_terminal_shows_the_preview(self):
        out = '\n'.join(text(dashboard.render(ROWS, State(), 100, 30, PREVIEW)))
        self.assertIn('preview', out)
        self.assertIn('Ran 235 tests', out)

    def test_footer_shows_key_hints(self):
        footer = text(dashboard.render(ROWS, State(), 120, 40, []))[-1]
        self.assertTrue(footer.startswith('└ enter jump'))
        self.assertIn('q quit', footer)

    def test_footer_shows_the_prompt_in_filter_mode(self):
        footer = text(dashboard.render(ROWS, State(mode='filter', text='inf'), 120, 40, []))[-1]
        self.assertIn('/inf', footer)

    def test_footer_shows_a_live_message(self):
        state = State(message="can't find pane", message_until=float('inf'))
        self.assertIn("can't find pane", text(dashboard.render(ROWS, state, 120, 40, []))[-1])

    def test_empty_table_says_so(self):
        self.assertIn('No panes', '\n'.join(text(dashboard.render([], State(), 80, 24, []))))

    def test_help_mode_lists_the_keys(self):
        out = '\n'.join(text(dashboard.render(ROWS, State(mode='help'), 100, 30, [])))
        self.assertIn('Jump to the pane', out)

    def test_colour_output_uses_the_status_palette(self):
        coloured = dashboard.frame_text(dashboard.render(ROWS, State(), 120, 40, []), color=True)
        self.assertIn('\x1b[', coloured)
        self.assertIn('38;2;247;118;142', coloured)  # #f7768e blocked
        self.assertTrue(coloured.endswith('\x1b[0m'))


class Golden(unittest.TestCase):
    def test_golden_frames(self):
        for w, h in ((120, 40), (80, 24), (50, 20)):
            with self.subTest(size=(w, h)):
                frame = dashboard.frame_text(dashboard.render(ROWS, State(), w, h, PREVIEW), color=False) + '\n'
                path = GOLDEN / f'dashboard-{w}x{h}.txt'
                if os.environ.get('UPDATE_GOLDEN') == '1':
                    path.write_text(frame)
                self.assertEqual(frame, path.read_text())


if __name__ == '__main__':
    unittest.main()
