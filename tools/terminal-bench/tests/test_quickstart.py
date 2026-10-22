"""Public Bash entry, automatic setup, presentation and application-fixture failures."""
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import xml.etree.ElementTree as ET

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import common
import daily
import quickstart
import report
import simple_report
import runner
from x11 import FocusLostError, X11


class Fixture(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='aura-launcher-test-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.font = self.root / 'font.ttf'
        self.font.write_bytes(b'font fixture')
        self.config = dict(font_family='DejaVu Sans Mono', font_file=str(self.font),
                           font_pixels=14, dpi=96, columns=80, rows=24, history=10000,
                           terminal=[dict(id=k, kind=k, binary='/bin/true', required=True)
                                     for k in ['aura', 'alacritty']])

    def manifest(self):
        return dict(schema=1, created_utc='fixture', config=self.config,
                    terminal_identities={k:dict(status='available', version='fixture', sha256=k)
                                         for k in ['aura', 'alacritty']},
                    settings=dict(repetitions=1), geometry_policy='grid', environment=dict(xvfb=True),
                    status='complete', trials=[dict(terminal=k, status='ok', warmup=False, round=0,
                                                   cell_pixels=[9, 17], metrics=dict(unicode_mib_s=[v]))
                                               for k, v in [('aura', 10), ('alacritty', 20)]])


class Entry(Fixture):
    def test_saved_report_prints_failure_causes_without_repeating_measurements(self):
        folder = self.root/'native'
        folder.mkdir()
        manifest = self.manifest()
        manifest['status'] = 'failed'
        manifest['trials'][0].update(status='failed', stage='window_geometry', error='PTY 142x53 != 80x24')
        source = folder/'run.json'
        source.write_text(json.dumps(manifest))
        original = source.read_bytes()
        with (patch.object(quickstart, 'show_report'), patch.object(quickstart.runner, 'run') as execute,
              contextlib.redirect_stdout(io.StringIO()) as output):
            self.assertEqual(quickstart.main(['--report', str(self.root), '--no-open']), 0)
        self.assertIn('window_geometry', output.getvalue())
        self.assertIn('PTY 142x53', output.getvalue())
        self.assertIn('terminal.log', output.getvalue())
        self.assertEqual(source.read_bytes(), original)
        execute.assert_not_called()

    def test_current_session_requires_display_without_silent_xvfb_fallback(self):
        with (patch.dict(os.environ, {'DISPLAY': ''}),
              patch.object(quickstart, 'discover', return_value=(self.config, [])),
              patch.object(quickstart.shutil, 'which', return_value='/bin/true'),
              patch.object(quickstart, 'preflight', return_value={}),
              patch.object(quickstart.runner, 'run') as execute,
              contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as errors):
            self.assertEqual(quickstart.main(['--current-session', '--output', str(self.root/'run')]), 1)
        execute.assert_not_called()
        self.assertIn('DISPLAY', errors.getvalue())
        self.assertFalse((self.root/'run').exists())

    def test_current_session_uses_inherited_display_for_both_campaigns(self):
        with (patch.dict(os.environ, DISPLAY=':123'),
              patch.object(quickstart, 'discover', return_value=(self.config, [])),
              patch.object(quickstart.shutil, 'which', return_value='/bin/true'),
              patch.object(quickstart, 'preflight', return_value={}),
              patch.object(quickstart, 'dependencies') as dependencies,
              patch.object(quickstart.adapters, 'identify', return_value=dict(status='available')),
              patch.object(quickstart.runner, 'run', return_value=dict(status='complete')) as native,
              patch.object(quickstart.daily, 'run', return_value=dict(status='complete')) as editor,
              patch.object(quickstart.report, 'render'), patch.object(quickstart, 'show_report'),
              contextlib.redirect_stdout(io.StringIO())):
            self.assertEqual(quickstart.main(['--current-session', '--nvim', '--output', str(self.root/'run')]), 0)
        dependencies.assert_called_once_with(False, True)
        for command in [native, editor]:
            self.assertEqual(command.call_args.kwargs['display'], ':123')
            self.assertFalse(command.call_args.kwargs['xvfb'])
        self.assertEqual(native.call_args.kwargs['environment'], 'unknown')

    def test_session_modes_are_mutually_exclusive(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            quickstart.parser().parse_args(['--current-session', '--display', ':123'])

    def test_bash_help_works_outside_checkout_and_without_import_dependencies(self):
        folder = self.root / 'working directory with spaces'
        folder.mkdir()
        command = common.ROOT / 'tools/benchmark.sh'
        result = subprocess.run(['/bin/bash', str(command), '--help'], cwd=folder,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('--nvim', result.stdout)
        self.assertIn('tools/benchmark.sh', result.stdout)

    def test_missing_python_has_actionable_error(self):
        command = common.ROOT / 'tools/benchmark.sh'
        result = subprocess.run(['/bin/bash', str(command)], env=dict(os.environ, PATH=str(self.root)),
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn('Python 3.11', result.stderr)

    def test_discovery_uses_actual_font_and_records_missing_emulators(self):
        with (patch.object(quickstart.subprocess, 'run', return_value=Mock(stdout=f'Other Mono\n{self.font}')),
              patch.object(quickstart.shutil, 'which', side_effect=lambda k: '/bin/true' if k == 'kitty' else None)):
            config, omitted = quickstart.discover('/bin/true')
        self.assertEqual(config['font_family'], 'Other Mono')
        self.assertEqual([e['kind'] for e in config['terminal']], ['aura', 'kitty'])
        self.assertNotIn('cell_spacing', config['terminal'][1])
        self.assertEqual({e['terminal'] for e in omitted}, {'alacritty', 'wezterm', 'xterm'})

    def test_generated_config_preserves_literal_paths_and_boolean_types(self):
        sentinel = self.root / 'must not exist'
        binary = self.root / ('literal $(touch must-not-exist) `true` "name"')
        self.config['terminal'][0]['binary'] = str(binary)
        path = self.root / 'configuration with spaces.toml'
        quickstart.write_config(path, self.config)
        loaded = common.load_config(path)
        self.assertEqual(loaded['terminal'][0]['binary'], str(binary))
        self.assertIs(loaded['terminal'][0]['required'], True)
        self.assertFalse(sentinel.exists())

    def test_preflight_requires_a_real_comparison_and_keeps_required_failures(self):
        available = dict(status='available')
        missing = dict(status='unavailable', error='runtime dependency missing')
        with patch.object(quickstart.adapters, 'identify_all', return_value=dict(aura=available, alacritty=missing)):
            with self.assertRaisesRegex(ValueError, 'runtime dependency missing'):
                quickstart.preflight(self.config)
            self.config['terminal'][1]['required'] = False
            with self.assertRaisesRegex(ValueError, 'otro emulador'):
                quickstart.preflight(self.config)

    def test_bad_bounds_fail_before_detection_or_mutation(self):
        for args in [['--repetitions', '0'], ['--warmups', '11'], ['--payload-mib', '257']]:
            with (patch.object(quickstart, 'discover') as discover,
                  contextlib.redirect_stderr(io.StringIO())):
                self.assertEqual(quickstart.main(args), 1)
                discover.assert_not_called()

    def test_list_is_diagnostic_even_with_fewer_than_two_emulators(self):
        with (patch.object(quickstart, 'discover', return_value=(self.config, [])),
              patch.object(quickstart.shutil, 'which', return_value='/bin/true'),
              patch.object(quickstart.adapters, 'identify_all', return_value={'aura':dict(status='available', version='fixture')}),
              patch.object(quickstart, 'preflight') as preflight,
              contextlib.redirect_stdout(io.StringIO())):
            self.assertEqual(quickstart.main(['--list']), 0)
        preflight.assert_not_called()

    def test_checkout_and_existing_output_are_rejected_before_run(self):
        existing = self.root / 'existing'
        existing.mkdir()
        sentinel = existing / 'evidence'
        sentinel.write_text('keep')
        repository = self.root / 'repository'
        (repository / '.git').mkdir(parents=True)
        (repository / '.git/HEAD').write_text('ref: refs/heads/main')
        link = self.root / 'symlink'
        link.symlink_to(repository, target_is_directory=True)
        for output in [common.ROOT / 'must-not-create', link / 'run', existing]:
            with (patch.object(quickstart, 'discover', return_value=(self.config, [])),
                  patch.object(quickstart.shutil, 'which', return_value='/bin/true'),
                  patch.object(quickstart, 'preflight'), patch.object(quickstart, 'dependencies'),
                  patch.object(quickstart.runner, 'run') as execute,
                  contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO())):
                self.assertEqual(quickstart.main(['--output', str(output), '--no-open']), 1)
                execute.assert_not_called()
        self.assertEqual(sentinel.read_text(), 'keep')
        self.assertFalse((common.ROOT / 'must-not-create').exists())

    def test_run_passes_settings_preserves_omissions_and_propagates_failure(self):
        folder = self.root / 'results with spaces'
        with (patch.object(quickstart, 'discover', return_value=(self.config, [dict(terminal='kitty', reason='absent')])),
              patch.object(quickstart.shutil, 'which', return_value='/bin/true'),
              patch.object(quickstart, 'preflight', return_value={}), patch.object(quickstart, 'dependencies'),
              patch.object(quickstart.runner, 'run', return_value=dict(status='failed')) as execute,
              patch.object(quickstart.report, 'render'), patch.object(quickstart, 'show_report') as show,
              contextlib.redirect_stdout(io.StringIO())):
            self.assertEqual(quickstart.main(['--quick', '--repetitions', '1', '--warmups', '0',
                                             '--no-open', '--output', str(folder)]), 1)
        settings = execute.call_args.args[1]
        self.assertEqual((settings['repetitions'], settings['warmups'], settings['profile']), (1, 0, 'quick'))
        self.assertTrue(execute.call_args.kwargs['xvfb'])
        show.assert_called_once_with(folder, True)
        self.assertEqual(json.loads((folder / 'launcher.json').read_text())['omissions'][0]['terminal'], 'kitty')
        self.assertEqual(common.load_config(folder / 'comparison.toml')['history'], 10000)

    def test_report_mode_needs_no_display_or_installed_emulators(self):
        with (patch.object(quickstart, 'show_report') as show,
              patch.object(quickstart, 'dependencies') as dependencies,
              patch.object(quickstart, 'discover') as discover):
            self.assertEqual(quickstart.main(['--report', str(self.root), '--no-open']), 0)
        show.assert_called_once_with(self.root, True)
        dependencies.assert_not_called()
        discover.assert_not_called()

    def test_browser_launch_failure_does_not_mark_good_measurements_failed(self):
        (self.root / 'native/report').mkdir(parents=True)
        def render(source, output, application):
            output.mkdir()
            path = output / 'index.html'
            path.write_text('report')
            return path
        with (patch.object(quickstart.simple_report, 'render', side_effect=render),
              patch.object(quickstart.shutil, 'which', return_value='/missing/xdg-open'),
              patch.dict(os.environ, DISPLAY=':999'),
              patch.object(quickstart.subprocess, 'Popen', side_effect=OSError('browser missing')) as browser,
              contextlib.redirect_stdout(io.StringIO())):
            path = quickstart.show_report(self.root, False)
        self.assertTrue(path.is_file())
        browser.assert_called_once()


class Presentation(Fixture):
    def test_light_chart_uses_same_scale_colors_and_marks_partial_data(self):
        rows = [dict(terminal=k, kind=k, status='partial' if k == 'aura' else 'complete', p50=1)
                for k in ['aura', 'alacritty']]
        document = ET.fromstring(simple_report.svg(rows, 'CPU < GPU', 'ms', False))
        rects = list(document.iter('{http://www.w3.org/2000/svg}rect'))
        self.assertEqual([r.attrib['fill'] for r in rects], ['#ffffff', '#4169ff', '#e7e7e7'])
        self.assertEqual(rects[1].attrib['height'], rects[2].attrib['height'])
        self.assertIn('1.000 *', ''.join(document.itertext()))
        self.assertIn('CPU < GPU', ''.join(document.itertext()))

    def test_simple_bars_scale_by_median_not_outliers_and_missing_stays_missing(self):
        rows = [dict(terminal=k, kind=k, status='complete', p50=v, max=10000, min=0)
                for k, v in [('aura', 10), ('alacritty', 20)]]
        svg = ET.fromstring(simple_report.svg(rows, 'title', 'ms', False))
        heights = [float(r.attrib['height']) for r in svg.iter('{http://www.w3.org/2000/svg}rect')][1:]
        self.assertEqual(heights, [75, 150])
        rows[0]['p50'] = None
        self.assertIn('sin datos', simple_report.svg(rows, 'title', 'ms', False))
        rows[0]['p50'] = 0
        self.assertIn('0.000', simple_report.svg(rows, 'title', 'ms', False))

    def test_native_and_editor_statistics_stay_separate_and_sources_are_hashed(self):
        native = self.manifest()
        source = self.root / 'native.json'
        source.write_text(json.dumps(native))
        app = self.manifest()
        app['nvim'] = dict(version='Neovim fixture')
        for trial in app['trials']:
            trial['metrics'] = dict(nvim_edit_continuous_ms=[5, 7])
        application = self.root / 'daily.json'
        application.write_text(json.dumps(app))
        with contextlib.redirect_stdout(io.StringIO()) as printed:
            path = simple_report.render(source, self.root / 'report', application)
        rows = json.loads((path.parent / 'summary.json').read_text())['rows']
        core = next(r for r in rows if r['terminal'] == 'aura' and r['metric'] == 'unicode_mib_s')
        editor = next(r for r in rows if r['terminal'] == 'aura' and r['metric'] == 'nvim_edit_continuous_ms')
        self.assertEqual((core['p50'], core['n']), (10, 1))
        self.assertEqual((editor['p50'], editor['n']), (5, 2))
        self.assertIn('#', printed.getvalue())
        self.assertIn('Detalles y percentiles', path.read_text())
        self.assertIn('Neovim fixture', path.read_text())
        overview = ET.parse(path.parent/'overview.svg')
        self.assertGreater(len(list(overview.iter('{http://www.w3.org/2000/svg}svg'))), 2)
        self.assertIn('Xvfb propio / pantalla virtual', path.read_text())
        with self.assertRaises(FileExistsError):
            simple_report.render(source, path.parent)

    def test_editor_cannot_be_combined_with_different_binaries(self):
        native = self.manifest()
        source = self.root / 'native.json'
        source.write_text(json.dumps(native))
        native['terminal_identities']['aura']['sha256'] = 'other'
        application = self.root / 'daily.json'
        application.write_text(json.dumps(native))
        with self.assertRaisesRegex(ValueError, 'different configurations or binaries'):
            simple_report.render(source, self.root / 'report', application)
        self.assertFalse((self.root / 'report').exists())

    def test_partial_rows_are_visible_in_console(self):
        stream = io.StringIO()
        simple_report.console([dict(cohort='grid', metric='unicode_mib_s', terminal='aura',
                                    status='partial', p50=1)], report.METRICS, stream)
        self.assertIn('partial', stream.getvalue())


class Editor(Fixture):
    def test_partial_control_record_waits_for_newline(self):
        path = self.root / 'events.jsonl'
        path.write_text('{"op":"ready"}\n{"op":')
        self.assertEqual(daily.events(path), [dict(op='ready')])

    def test_old_matching_navigation_event_cannot_acknowledge_new_action(self):
        path = self.root / 'events.jsonl'
        path.write_text('{"op":"cursor","top":1}\n')
        process, sampler = Mock(), Mock()
        process.poll.return_value = 0
        with self.assertRaisesRegex(RuntimeError, 'terminal exited'):
            daily.wait_event(path, lambda e: e['top'] == 1, sampler, process, 1, after=1)

    def test_failed_native_run_cannot_supply_editor_geometry(self):
        native = self.manifest()
        native['status'] = 'failed'
        path = self.root / 'native.json'
        path.write_text(json.dumps(native))
        with self.assertRaisesRegex(ValueError, 'complete native run'):
            daily.run(path, self.root / 'editor', {})
        self.assertFalse((self.root / 'editor').exists())


class Session(unittest.TestCase):
    def test_tiled_grids_and_fractional_tails_are_pinned_before_measurement(self):
        for columns, rows, width, height, cell in [(142,53,1136,1060,[8,20]),
                (126,53,1142,1064,[9,20]), (114,53,1142,1064,[10,20])]:
            with self.subTest(columns=columns):
                x11, channel = Mock(), Mock()
                target = [80*cell[0], 24*cell[1]]
                x11.geometry.side_effect = [[width,height], target]
                channel.request.side_effect = [dict(size=dict(columns=columns,rows=rows,pixel_width=width,pixel_height=height)),
                                               dict(size=dict(columns=80,rows=24,pixel_width=target[0],pixel_height=target[1]))]
                result = runner.prepare_geometry(x11, 42, channel, dict(columns=80,rows=24), 1)
                self.assertEqual(result['cell_pixels'], cell)
                self.assertEqual(result['window_pixels'], target)
                self.assertEqual(result['normalized_pty']['columns'], 80)
                x11.prepare_window.assert_called_once_with(42,1)

    def test_normalization_rejects_wrong_pty_and_client_geometry(self):
        for actual_grid, actual_pixels in [([81,24],[640,480]),([80,24],[641,480])]:
            x11, channel = Mock(), Mock()
            x11.geometry.side_effect = [[1136,1060], actual_pixels, actual_pixels]
            channel.request.side_effect = [dict(size=dict(columns=142,rows=53,pixel_width=1136,pixel_height=1060)),
                                           dict(size=dict(columns=actual_grid[0],rows=actual_grid[1]))]
            with self.assertRaisesRegex(ValueError, 'normalization failed'):
                runner.prepare_geometry(x11,42,channel,dict(columns=80,rows=24),1)

    def test_progress_prints_stage_cause_and_log_without_terminal_controls(self):
        result = dict(terminal='aura',round=0,warmup=True,status='failed',stage='startup',error='bad\x1b[31m config')
        with contextlib.redirect_stdout(io.StringIO()) as output:
            runner.trial_progress(result, Path('/private/trial'))
        self.assertIn('[startup]: bad', output.getvalue())
        self.assertIn('/private/trial/terminal.log', output.getvalue())
        self.assertNotIn('\x1b', output.getvalue())

    def test_default_xauthority_path_survives_home_isolation_without_copying_cookies(self):
        with tempfile.TemporaryDirectory(prefix='aura-authority-') as folder:
            home = Path(folder)
            (home/'.Xauthority').touch()
            with patch.dict(os.environ, HOME=str(home)):
                os.environ.pop('XAUTHORITY', None)
                environment = runner.display_environment(':123', HOME='/isolated/home')
                self.assertEqual(environment['XAUTHORITY'], str(home/'.Xauthority'))
                self.assertEqual(environment['HOME'], '/isolated/home')
                self.assertNotIn('XAUTHORITY', os.environ)
                os.environ['XAUTHORITY'] = '/explicit/authority'
                self.assertEqual(runner.display_environment(':123')['XAUTHORITY'], '/explicit/authority')

    def test_device_inventory_distinguishes_software_hardware_mixed_and_unknown(self):
        gl = 'OpenGL renderer string: AMD Radeon\n    Accelerated: yes\n'
        self.assertEqual(runner.renderer_inventory('gl', gl, 0)['classification'], 'hardware')
        self.assertEqual(runner.renderer_inventory('gl', gl.replace('AMD Radeon', 'llvmpipe'), 0)['classification'], 'software')
        self.assertEqual(runner.renderer_inventory('gl', gl, 1)['classification'], 'unknown')
        gpu = 'deviceName = Radeon\ndeviceType = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU\n'
        cpu = 'deviceName = llvmpipe\ndeviceType = PHYSICAL_DEVICE_TYPE_CPU\n'
        for text, expected in [(gpu,'hardware'), (cpu,'software'), (gpu+cpu,'mixed'), ('','unknown')]:
            self.assertEqual(runner.renderer_inventory('vulkan', text, 0)['classification'], expected)
        label = simple_report.environment_label(dict(xvfb=False, vulkan=runner.renderer_inventory('vulkan', gpu+cpu, 0)))
        self.assertIn('mixed', label)
        self.assertNotIn('Xvfb propio', label)

    def connection(self, focused):
        connection = X11.__new__(X11)
        connection.x, connection.xt = Mock(), Mock()
        connection.display, connection.target, connection.keycode = 1, 42, 38
        connection.previous_focus = 12
        connection.focused, connection.sync = Mock(return_value=focused), Mock()
        return connection

    def test_focus_loss_sends_no_keys_and_always_releases_server(self):
        connection = self.connection(99)
        with self.assertRaises(FocusLostError):
            connection.key()
        connection.xt.XTestFakeKeyEvent.assert_not_called()
        connection.x.XUngrabServer.assert_called_once_with(1)
        connection.restore_focus()
        connection.x.XSetInputFocus.assert_not_called()

    def test_delivery_failure_releases_server_and_prior_focus_is_restored_only_when_owned(self):
        connection = self.connection(42)
        connection.xt.XTestFakeKeyEvent.return_value = 0
        with self.assertRaisesRegex(RuntimeError, 'injection'):
            connection.key()
        connection.x.XUngrabServer.assert_called_once_with(1)
        connection.restore_focus()
        connection.x.XSetInputFocus.assert_called_once_with(1, 12, 2, 0)
        self.assertIsNone(connection.target)

    def test_chord_failure_releases_pressed_modifier_and_server(self):
        connection = self.connection(42)
        connection.x.XKeysymToKeycode.side_effect = [37, 41]
        connection.xt.XTestFakeKeyEvent.side_effect = [1, 0, 1]
        with self.assertRaisesRegex(RuntimeError, 'injection'):
            connection.keys('ctrl+f')
        self.assertEqual([call.args[1:3] for call in connection.xt.XTestFakeKeyEvent.call_args_list],
                         [(37, True), (41, True), (37, False)])
        connection.x.XUngrabServer.assert_called_once_with(1)


class Warmups(Fixture):
    def test_native_failed_warmup_retains_failures_and_stops_before_measured_rounds(self):
        identities = self.manifest()['terminal_identities']
        def trial(entry, *args):
            return dict(terminal=entry['id'], status='failed', stage='window_geometry', error='bad grid',metrics={})
        with (patch.object(runner.subprocess, 'run', return_value=Mock(returncode=0,stdout=str(self.font).encode())),
              patch.object(runner.adapters, 'identify_all', return_value=identities),
              patch.object(runner, 'display_session', return_value=contextlib.nullcontext(':test')),
              patch.object(runner, 'inventory', return_value={}), patch.object(runner, 'X11'),
              patch.object(runner, 'trial', side_effect=trial) as execute,
              contextlib.redirect_stdout(io.StringIO()) as output):
            result = runner.run(self.config,dict(seed=1,warmups=2,repetitions=7),self.root/'run')
        self.assertEqual(result['status'],'failed')
        self.assertTrue(result['stopped_after_failed_warmup'])
        self.assertEqual(execute.call_count,2)
        self.assertTrue(all(t['warmup'] and t['error']=='bad grid' for t in result['trials']))
        self.assertIn('Calentamiento fallido',output.getvalue())

    def test_editor_failed_warmup_retains_failures_and_stops_before_measured_rounds(self):
        native=self.manifest()
        native['settings'].update(seed=1,warmups=2,repetitions=7)
        source=self.root/'native.json';source.write_text(json.dumps(native))
        def trial(entry,*args):
            return dict(terminal=entry['id'],status='failed',error='no editor geometry',metrics={})
        with (patch.object(daily,'display_session',return_value=contextlib.nullcontext(':test')),
              patch.object(daily,'inventory',return_value={}),patch.object(daily,'X11'),
              patch.object(daily,'trial',side_effect=trial) as execute,
              contextlib.redirect_stdout(io.StringIO())):
            result=daily.run(source,self.root/'editor',dict(version='fixture'))
        self.assertEqual(result['status'],'failed')
        self.assertTrue(result['stopped_after_failed_warmup'])
        self.assertEqual(execute.call_count,2)
        self.assertTrue(all(t['warmup'] for t in result['trials']))
