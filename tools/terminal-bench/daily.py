"""Optional real-Neovim edits with an independent, observed status-line witness."""
import json
from datetime import datetime, timezone
import os
from pathlib import Path
import random
import shutil
import subprocess
import time
import uuid

import adapters
from common import outside_checkout, require_hash, sha256
from resources import Resources
from runner import display_environment, display_session, input_color, inventory, save, stop, wait_pixel, window_for
from x11 import FocusLostError, X11

HERE = Path(__file__).resolve().parent


def events(path):
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines(keepends=True)
            if line.endswith('\n')]


def wait_event(path, predicate, resources, process, timeout, after=0):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        for event in events(path)[after:]:
            if predicate(event):
                return event
        if process.poll() is not None:
            raise RuntimeError(f'terminal exited {process.returncode} during Neovim')
        resources.sample()
        time.sleep(.001)
    raise TimeoutError('Neovim state not reached; inspect events.jsonl and terminal.log')


def trial(entry, identity, directory, native, nvim, display, x11):
    x11.begin_trial()
    directory.mkdir()
    for name in ['home', 'config', 'data', 'cache', 'state']:
        (directory / name).mkdir()
    config, settings = native['config'], native['settings']
    timeout = settings['timeout']
    path = directory / 'events.jsonl'
    title = 'AURA_NVIM_' + uuid.uuid4().hex
    command = adapters.launch(entry['kind'], identity['binary'], directory, config,
                              [nvim['binary'], '--noplugin', '-n', '-u', str(directory.parent / 'nvim_fixture.lua'),
                               '-i', 'NONE', str(directory.parent / 'fixture.rs')], title,
                              entry.get('cell_spacing', [0, 0]))
    env = display_environment(display, HOME=str(directory / 'home'),
               XDG_CONFIG_HOME=str(directory / 'config'), XDG_DATA_HOME=str(directory / 'data'),
               XDG_CACHE_HOME=str(directory / 'cache'), XDG_STATE_HOME=str(directory / 'state'),
               FONTCONFIG_FILE=str(Path(native['font_config_path'])), LANG='C.UTF-8', LC_ALL='C.UTF-8',
               WINIT_UNIX_BACKEND='x11', WINIT_X11_SCALE_FACTOR=str(config['dpi'] / 96),
               NVIM_LOG_FILE=str(directory / 'nvim.log'), AURA_BENCH_NVIM_EVENTS=str(path),
               AURA_BENCH_NVIM_TITLE=title)
    for name in ['WAYLAND_DISPLAY', 'AURA_PERF', 'AURA_DEBUG_KEYBOARD', 'WEZTERM_UNIX_SOCKET',
                 'KITTY_LISTEN_ON', 'TERM_PROGRAM', 'TERM_PROGRAM_VERSION', 'WINDOWID']:
        env.pop(name, None)
    result = dict(terminal=entry['id'], status='failed', metrics={}, phases={}, command=command)
    process = None
    try:
        require_hash(identity['binary'], identity['sha256'])
        require_hash(nvim['binary'], nvim['sha256'])
        require_hash(config['font_file'], native['font_sha256'])
        started = time.monotonic_ns()
        with (directory / 'terminal.log').open('wb') as log:
            process = subprocess.Popen(command, env=env, cwd=directory, stdout=log, stderr=log,
                                       start_new_session=True)
        resources = Resources(process.pid)
        ready = wait_event(path, lambda e: e['op'] == 'ready', resources, process, timeout)
        if ready['text'] != '' or not ready['mode'].startswith('i') or [ready['columns'], ready['rows']] != [config['columns'], config['rows']]:
            raise ValueError(f'Neovim initial state differs: {ready}')
        resources.exclude.add(ready['pid'])
        resources.ticks = {key: value for key, value in resources.ticks.items() if key[0] != ready['pid']}
        resources.peak = 0
        window = window_for(title, display, resources, timeout)
        result['initial_window_pixels'] = x11.geometry(window)
        # Reuse measured raw-PTY geometry, not guessed font-name metrics.
        reference = next(t for t in native['trials'] if t['terminal'] == entry['id'] and t['status'] == 'ok')
        cw, ch = reference['cell_pixels']
        width, height = cw * config['columns'], ch * config['rows']
        if x11.geometry(window) != [width, height]:
            x11.resize(window, width, height)
            x11.sync()
        if x11.geometry(window) != [width, height]:
            raise ValueError('window geometry normalization failed')
        result['cell_pixels'] = [cw, ch]
        x11.focus(window)
        point = [width - max(1, cw // 2), height - 2 * ch + ch // 2]
        observed = wait_pixel(x11, window, point, [41, 83, 167], resources, timeout)
        result['metrics']['nvim_startup_ms'] = [(observed - started) / 1e6]
        index, gaps = 0, []
        samples = 8 if settings['profile'] == 'quick' else 20
        for name, pause in [('continuous', 0), ('after_pause', .05)]:
            values, phase = [], resources.begin()
            for _ in range(samples):
                if pause:
                    time.sleep(pause)
                index += 1
                before = x11.key()
                if pause:
                    gaps.append((before - observed) / 1e6)
                event = wait_event(path, lambda e: e['op'] == 'edit' and e['index'] == index,
                                   resources, process, timeout)
                if event['text'] != 'a' * index or not event['mode'].startswith('i') or event['rgb'] != input_color(index):
                    raise ValueError(f'Neovim edited unexpected input: {event}')
                if [event['columns'], event['rows']] != [config['columns'], config['rows']]:
                    raise ValueError('Neovim grid changed during input')
                observed = wait_pixel(x11, window, point, event['rgb'], resources, timeout)
                values.append((observed - before) / 1e6)
            result['phases'][name] = resources.end(phase)
            result['metrics']['nvim_edit_' + name + '_ms'] = values
        result['paused_gaps_ms'] = gaps
        if any(g < 50 for g in gaps):
            raise ValueError('requested input pause not reached')
        keys = x11.keys
        before = len(events(path))
        keys('Escape', 'ctrl+f')
        down = wait_event(path, lambda e: e['op'] == 'cursor' and e['top'] > 1,
                          resources, process, timeout, before)
        before = len(events(path))
        keys('ctrl+b')
        up = wait_event(path, lambda e: e['op'] == 'cursor' and e['top'] < down['top'],
                        resources, process, timeout, before)
        before = len(events(path))
        # Explicit keysyms avoid changing the session's keymap or modifiers.
        keys('slash', 'f', 'n', 'space', *list('benchmark'), 'shift+minus', *list('target'))
        keys('Return')
        found = wait_event(path, lambda e: e['op'] == 'cursor' and e['text'] == 'fn benchmark_target() {}',
                           resources, process, timeout, before)
        result['navigation'] = dict(page_down_top=down['top'], page_up_top=up['top'], search_line=found['row'])
        keys('Escape', 'shift+semicolon', 'q', 'a', 'shift+1', 'Return')
        x11.restore_focus()
        if process.wait(timeout=timeout):
            raise RuntimeError('terminal exited unsuccessfully after Neovim')
        require_hash(identity['binary'], identity['sha256'])
        require_hash(nvim['binary'], nvim['sha256'])
        require_hash(config['font_file'], native['font_sha256'])
        result.update(status='ok', checks=dict(exact_buffer=True, observed_witness=True, navigation=True))
    except (OSError, ValueError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
        if isinstance(error, FocusLostError):
            raise
        result['error'] = str(error)
    finally:
        x11.restore_focus()
        stop(process)
    return result


def run(native_path, output, nvim, display=None, xvfb=True):
    native_path = outside_checkout(native_path)
    native = json.loads(native_path.read_text())
    if native['status'] != 'complete':
        raise ValueError('Neovim requires a complete native run for verified geometry')
    native['font_config_path'] = str(native_path.parent / 'fonts.conf')
    output = outside_checkout(output)
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    for name in ['daily.py', 'nvim_fixture.lua', 'adapters.py', 'common.py', 'runner.py', 'resources.py', 'x11.py']:
        shutil.copyfile(HERE / name, output / name)
    (output / 'fixture.rs').write_text('\n' + ''.join(f'// line {i}\n' for i in range(1, 1000)) +
                                     'fn benchmark_target() {}\n' + '// more text\n' * 1000)
    manifest = dict(schema=1, created_utc=datetime.now(timezone.utc).isoformat(), config=native['config'],
                    settings=native['settings'], geometry_policy=native['geometry_policy'],
                    terminal_identities=native['terminal_identities'], nvim=nvim, trials=[], status='running',
                    source_native_sha256=sha256(native_path), fixture_sha256=sha256(output / 'fixture.rs'),
                    source_sha256={p.name: sha256(p) for p in output.iterdir() if p.is_file()},
                    methodology='Instrumented real Neovim; exact buffer + fresh status-line witness. '
                    'Includes editor/callback/control/poll costs; not glyph scanout or input-to-photon. '
                    'Page and search checks are outside edit clocks. No personal plugins/LSP.')
    save(output / 'run.json', manifest)
    try:
        with display_session(display, xvfb, output, native['config']) as selected:
            manifest['environment'] = inventory(selected, output) | dict(xvfb=xvfb)
            generator, x11 = random.Random(native['settings']['seed']), X11(selected)
            try:
                for index in range(native['settings']['warmups'] + native['settings']['repetitions']):
                    entries = [e for e in native['config']['terminal'] if native['terminal_identities'][e['id']]['status'] == 'available']
                    generator.shuffle(entries)
                    for entry in entries:
                        for name, expected in manifest['source_sha256'].items():
                            require_hash(output / name, expected)
                        manifest['active_trial'] = dict(terminal=entry['id'], round=index)
                        save(output / 'run.json', manifest)
                        result = trial(entry, native['terminal_identities'][entry['id']], output / f'trial-{index:03}-{entry["id"]}', native, nvim, selected, x11)
                        for name, expected in manifest['source_sha256'].items():
                            require_hash(output / name, expected)
                        result.update(round=index, warmup=index < native['settings']['warmups'])
                        manifest['trials'].append(result)
                        manifest.pop('active_trial', None)
                        save(output / 'run.json', manifest)
                        print(f'Neovim: {entry["id"]} round={index} {result["status"]}', flush=True)
            finally:
                x11.close()
        manifest['status'] = 'complete' if all(t['status'] == 'ok' for t in manifest['trials']) else 'failed'
    except BaseException as error:
        manifest.update(status='interrupted' if isinstance(error, KeyboardInterrupt) else 'failed', error=str(error))
        raise
    finally:
        save(output / 'run.json', manifest)
    return manifest
