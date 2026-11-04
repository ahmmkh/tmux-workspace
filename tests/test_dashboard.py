"""The dashboard's pure renderer and key handling. Runs on Python 3.9: no enterContext."""
import contextlib
import dataclasses
import io
import os
import subprocess
from pathlib import Path
import sys
import unittest
from unittest import mock

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


class ShortPath(unittest.TestCase):
    def test_home_through_a_symlink_still_shortens(self):
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            real = Path(directory).resolve() / 'real'
            (real / 'code').mkdir(parents=True)
            link = Path(directory) / 'link'
            link.symlink_to(real)
            with mock.patch.object(dashboard.Path, 'home', return_value=link):
                self.assertEqual(dashboard.short_path(str(real / 'code')), '~/code')
                self.assertEqual(dashboard.short_path(str(link / 'code')), '~/code')
                self.assertEqual(dashboard.short_path('/srv/elsewhere'), '/srv/elsewhere')


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


PARKED = [r for r in ROWS if r.parked][0]


class Keys(unittest.TestCase):
    def setUp(self):
        self.ws = {'tm': mock.Mock(return_value=''), 'park': mock.Mock(), 'rejoin': mock.Mock(), 'set_label': mock.Mock()}

    def press(self, keys, state=None, rows=ROWS):
        state = state or State(origin='%9')
        for key in keys:
            state = dashboard.handle_key(key, state, rows, self.ws)
        return state

    def test_down_then_enter_jumps_to_second_row(self):
        state = self.press(['KEY_DOWN', '\n'])
        pane = ROWS[1].pane_id
        self.ws['tm'].assert_any_call('select-window', '-t', pane)
        self.ws['tm'].assert_any_call('select-pane', '-t', pane)
        self.ws['tm'].assert_any_call('switch-client', '-t', pane, check=False)
        self.assertTrue(state.quit)

    def test_j_k_g_and_G_move(self):
        self.assertEqual(self.press(['j', 'j']).selected, ROWS[2].pane_id)
        self.assertEqual(self.press(['j', 'j', 'k']).selected, ROWS[1].pane_id)
        self.assertEqual(self.press(['G']).selected, ROWS[-1].pane_id)
        self.assertEqual(self.press(['G', 'g']).selected, ROWS[0].pane_id)
        self.assertEqual(self.press(['k']).selected, ROWS[0].pane_id)

    def test_enter_on_parked_row_rejoins_next_to_origin(self):
        state = self.press(['\n'], State(origin='%9', selected=PARKED.pane_id))
        self.ws['rejoin'].assert_called_once_with(PARKED.pane_id, '%9')
        self.assertTrue(state.quit)

    def test_park_refuses_origin_pane(self):
        state = self.press(['p'], State(origin=ROWS[0].pane_id))
        self.ws['park'].assert_not_called()
        self.assertIn('Cannot park the pane this dashboard was opened from', state.message)

    def test_park_selected_pane(self):
        self.press(['j', 'p'])
        self.ws['park'].assert_called_once_with(ROWS[1].pane_id)

    def test_unpark_needs_a_parked_row(self):
        self.assertIn('not parked', self.press(['u']).message)
        self.press(['u'], State(origin='%9', selected=PARKED.pane_id))
        self.ws['rejoin'].assert_called_once_with(PARKED.pane_id, '%9')

    def test_rename_prompt_sets_label(self):
        state = self.press(['r'] + ['KEY_BACKSPACE'] * 10 + list('logs') + ['\n'])
        self.ws['set_label'].assert_called_once_with(ROWS[0].pane_id, 'logs')
        self.assertEqual(state.mode, 'normal')

    def test_rename_escape_cancels(self):
        self.press(['r', 'x', '\x1b'])
        self.ws['set_label'].assert_not_called()

    def test_kill_needs_y(self):
        self.press(['x', 'n'])
        self.assertNotIn(mock.call('kill-pane', '-t', ROWS[0].pane_id), self.ws['tm'].call_args_list)
        self.press(['x', 'y'])
        self.ws['tm'].assert_called_with('kill-pane', '-t', ROWS[0].pane_id)

    def test_kill_targets_the_pane_chosen_when_x_was_pressed(self):
        state = self.press(['j', 'x'])
        reordered = [ROWS[2], ROWS[1], ROWS[0]] + ROWS[3:]  # a refresh moves rows while the question is open
        state.selected = reordered[0].pane_id
        dashboard.handle_key('y', state, reordered, self.ws)
        self.ws['tm'].assert_called_with('kill-pane', '-t', ROWS[1].pane_id)

    def test_kill_does_nothing_when_the_chosen_pane_is_gone(self):
        state = self.press(['j', 'x'])
        remaining = [r for r in ROWS if r.pane_id != ROWS[1].pane_id]
        state = dashboard.handle_key('y', state, remaining, self.ws)
        self.assertNotIn('kill-pane', str(self.ws['tm'].call_args_list))
        self.assertIn('gone', state.message)

    def test_rename_targets_the_pane_chosen_when_r_was_pressed(self):
        state = self.press(['j', 'r', 'x'])
        state.selected = ROWS[0].pane_id
        dashboard.handle_key('\n', state, ROWS, self.ws)
        self.ws['set_label'].assert_called_once_with(ROWS[1].pane_id, ROWS[1].label + 'x')

    def test_kill_prompt_names_the_pane(self):
        state = self.press(['x'])
        footer = text(dashboard.render(ROWS, state, 120, 40, []))[-1]
        self.assertIn('close ' + ROWS[0].label + '?', footer)

    def test_filter_is_live_and_escape_clears(self):
        state = self.press(['/', 'i', 'n', 'f'])
        self.assertEqual((state.mode, state.query), ('filter', 'inf'))
        self.assertEqual(self.press(['\n'], state).query, 'inf')
        self.assertEqual(self.press(['\x1b'], state).query, '')

    def test_escape_clears_a_filter_before_quitting(self):
        state = self.press(['/', 'x', '\n', '\x1b'])
        self.assertEqual((state.query, state.quit), ('', False))
        self.assertTrue(self.press(['\x1b'], state).quit)

    def test_a_toggles_agents_only(self):
        self.assertTrue(self.press(['a']).agents_only)
        self.assertFalse(self.press(['a', 'a']).agents_only)

    def test_help_opens_and_any_key_closes(self):
        self.assertEqual(self.press(['?']).mode, 'help')
        self.assertEqual(self.press(['?', 'j']).mode, 'normal')

    def test_vanished_pane_error_reaches_footer(self):
        self.ws['tm'].side_effect = subprocess.CalledProcessError(1, 'tmux', stderr="can't find pane: %1\n")
        state = self.press(['\n'])
        self.assertIn("can't find pane", state.message)
        self.assertFalse(state.quit)

    def test_keys_on_an_empty_table_do_nothing(self):
        state = self.press(['j', '\n', 'p', 'x', 'y', 'r', '\n'], rows=[])
        self.ws['park'].assert_not_called()
        self.ws['tm'].assert_not_called()
        self.assertFalse(state.quit)


class Main(unittest.TestCase):
    def test_main_outside_tmux_exits_1_without_traceback(self):
        ws = {'tm': mock.Mock(side_effect=subprocess.CalledProcessError(1, 'tmux', stderr='no server running'))}
        err = io.StringIO()
        with mock.patch.dict(os.environ, {}, clear=False), contextlib.redirect_stderr(err):
            os.environ.pop('TMUX', None)
            code = dashboard.main(ws, ['--once'])
        self.assertEqual(code, 1)
        self.assertEqual(err.getvalue().strip(), 'tmux-workspace dash: no tmux server is running (start one with: work)')

    def test_once_prints_one_frame_of_the_requested_size(self):
        ws = {'tm': mock.Mock(return_value=''), 'fields': mock.Mock(return_value=FIXTURE)}
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = dashboard.main(ws, ['--once', '--width', '90', '--height', '12'])
        lines = out.getvalue().rstrip('\n').split('\n')
        self.assertEqual((code, len(lines)), (0, 12))
        self.assertEqual({dashboard.cell_width(line) for line in lines}, {90})
        self.assertIn('infra', out.getvalue())

    def test_once_does_not_read_terminal_text_it_will_not_show(self):
        ws = {'tm': mock.Mock(return_value=''), 'fields': mock.Mock(return_value=FIXTURE)}
        with contextlib.redirect_stdout(io.StringIO()):
            dashboard.main(ws, ['--once', '--height', '19'])
        self.assertNotIn('capture-pane', str(ws['tm'].call_args_list))

    def test_preview_strips_control_sequences_and_trailing_blanks(self):
        ws = {'tm': mock.Mock(return_value='one\n\x1b[31mtwo\x07\n\n\n')}
        self.assertEqual(dashboard.read_preview(ws, '%1', 5), ['one', 'two'])
        ws['tm'].side_effect = subprocess.CalledProcessError(1, 'tmux')
        self.assertEqual(dashboard.read_preview(ws, '%1', 5), [])


if __name__ == '__main__':
    unittest.main()
