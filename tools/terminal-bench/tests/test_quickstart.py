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
