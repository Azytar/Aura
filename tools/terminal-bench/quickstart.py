"""Implementation of the Bash entry point; no Python configuration is required."""
import argparse
import ctypes.util
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import uuid

import adapters
from common import KINDS, PROFILES, ROOT, load_config, outside_checkout
import daily
import report
import runner
import simple_report


def parser():
    cli = argparse.ArgumentParser(prog='tools/benchmark.sh', description='Compara Aura con otros terminales y muestra barras gráficas.')
    cli.add_argument('--quick', action='store_true', help='prueba corta de funcionamiento; no sirve para rankings estables')
    cli.add_argument('--output', type=Path, help='carpeta NUEVA fuera de Git; por defecto ~/.local/state/aura/benchmarks/')
    cli.add_argument('--config', type=Path, help='TOML personalizado; normalmente no hace falta')
    cli.add_argument('--aura', help='ruta al binario Aura; por defecto target/release/aura o PATH')
    cli.add_argument('--build', action='store_true', help='compilar Aura en release antes de medir')
    cli.add_argument('--font', help='familia Fontconfig; por defecto DejaVu Sans Mono o su sustituta disponible')
    session = cli.add_mutually_exclusive_group()
    session.add_argument('--display', help='servidor X11 específico, p. ej. :1')
    session.add_argument('--current-session', action='store_true', help='usar DISPLAY de tu escritorio y sus drivers; abre ventanas e inyecta teclas')
    cli.add_argument('--geometry-policy', choices=['grid', 'cell'], default='grid', help='grid: mismas columnas; cell: mismas dimensiones de celda')
    cli.add_argument('--nvim', action='store_true', help='añadir escritura, paginado y búsqueda en Neovim real (requiere nvim)')
    cli.add_argument('--no-open', action='store_true', help='mostrar barras en consola y guardar HTML, sin abrir navegador')
    cli.add_argument('--list', action='store_true', help='mostrar emuladores y fuente detectados, sin medir')
    cli.add_argument('--report', type=Path, help='volver a mostrar barras de una carpeta anterior, sin repetir las mediciones')
    cli.add_argument('--repetitions', type=int, help='repeticiones medidas (1..100)')
    cli.add_argument('--warmups', type=int, help='calentamientos (0..10)')
    cli.add_argument('--payload-mib', type=int, help='presupuesto por carga (1..256 MiB)')
    return cli


def discover(aura=None, font=None, *, allow_missing_aura=False):
    match = subprocess.run(['fc-match', '-f', '%{family}\n%{file}', font or 'DejaVu Sans Mono'],
                           capture_output=True, text=True, check=True, timeout=10)
    family, path = match.stdout.strip().split('\n', 1)
    family = family.split(',')[0]
    config = dict(font_family=family, font_file=str(Path(path).resolve(strict=True)),
                  font_pixels=14.0, dpi=96.0, columns=80, rows=24, history=10000, terminal=[])
    omissions = []
    for kind in KINDS:
        if kind == 'aura':
            local = ROOT / 'target/release/aura'
            binary = aura if aura else (str(local) if local.is_file() and os.access(local, os.X_OK) else shutil.which('aura'))
        else:
            binary = shutil.which(kind)
        if not binary:
            if kind == 'aura' and not allow_missing_aura:
                raise ValueError('No se encuentra Aura. Usa --build o --aura /ruta/al/binario.')
            omissions.append(dict(terminal=kind, reason='no instalado'))
            continue
        entry = dict(id=kind, kind=kind, binary=str(Path(binary).resolve()), required=True)
        # This fixture is verified for DejaVu/14px. Other fonts keep native spacing.
        if kind in ['alacritty', 'kitty'] and family == 'DejaVu Sans Mono':
            entry['cell_spacing'] = [1, 0]
        config['terminal'].append(entry)
    return config, omissions


def write_config(path, config):
    text = '\n'.join(f'{key}={json.dumps(value, ensure_ascii=False)}' for key, value in config.items() if key != 'terminal') + '\n'
    for entry in config['terminal']:
        text += '\n[[terminal]]\n' + '\n'.join(
            f'{key}={json.dumps(value, ensure_ascii=False)}' for key, value in entry.items()) + '\n'
    path.write_text(text)


def dependencies(xvfb, nvim=False):
    required = ['fc-match', 'xdotool'] + (['Xvfb'] if xvfb else []) + (['nvim'] if nvim else [])
    missing = [name for name in required if not shutil.which(name)]
    missing += [name for name in ['X11', 'Xtst'] if not ctypes.util.find_library(name)]
    if missing:
        raise ValueError('Faltan dependencias: ' + ', '.join(missing) + '. Consulta docs/TERMINAL_BENCHMARKS.md.')


def preflight(config):
    identities = adapters.identify_all(config['terminal'])
    failed = [name + ': ' + v.get('error', '') for name, v in identities.items()
              if v['status'] != 'available' and next(e['required'] for e in config['terminal'] if e['id'] == name)]
    if failed:
        raise ValueError('No se pueden ejecutar estos terminales: ' + '; '.join(failed))
    if sum(v['status'] == 'available' for v in identities.values()) < 2:
        raise ValueError('Instala al menos otro emulador además de Aura: Alacritty, Kitty, WezTerm o xterm.')
    return identities


def show_report(session, no_open):
    native = session / 'native/run.json'
    application = session / 'neovim/run.json'
    if not (session / 'native/report').exists():
        report.render(native, session / 'native/report')
    view = session / ('bars-' + datetime.now(timezone.utc).strftime('%Y%m%d-%H%M%S') + '-' + uuid.uuid4().hex[:8])
    path = simple_report.render(native, view, application if application.is_file() else None)
    print(f'\nBarras HTML: {path}\nDatos y registros privados: {session}', flush=True)
    if not no_open:
        opener = shutil.which('xdg-open')
        if opener and (os.environ.get('DISPLAY') or os.environ.get('WAYLAND_DISPLAY')):
            try:
                with (view / 'browser.log').open('wb') as log:
                    subprocess.Popen([opener, path.as_uri()], stdout=log, stderr=log, start_new_session=True)
                print('Abriendo el informe en el navegador. Si no aparece, abre la ruta HTML indicada.')
            except OSError as error:
                print(f'No se pudo abrir el navegador ({error}); abre la ruta HTML indicada.')
        else:
            print('Sin navegador gráfico disponible; las barras se muestran arriba y el HTML queda guardado.')
    return path


def show_failures(session):
    for name in ['native', 'neovim']:
        source = outside_checkout(session / name / 'run.json')
        if not source.is_file():
            continue
        manifest = json.loads(source.read_text())
        if manifest.get('status') != 'complete':
            print(f'{name}: {manifest.get("status", "unknown")} · {manifest.get("error", "")}', flush=True)
        for trial in manifest.get('trials', []):
            if trial['status'] != 'ok':
                directory = source.parent / trial.get('directory', f'trial-{trial["round"]:03}-{trial["terminal"]}')
                runner.trial_progress(trial, directory, prefix=name + ': ')


def main(argv=None):
    cli = parser()
    args = cli.parse_args(argv)
    try:
        if sys.platform != 'linux':
            raise ValueError('Este benchmark necesita Linux/X11.')
        if args.report:
            session = outside_checkout(args.report)
            show_failures(session)
            show_report(session, args.no_open)
            return 0
        if args.config and (args.aura or args.font or args.build):
            raise ValueError('--config no se combina con --aura, --font ni --build.')
        profile = 'quick' if args.quick else 'standard'
        settings = PROFILES[profile] | dict(seed=0x41555241, timeout=30, profile=profile)
        for field in ['repetitions', 'warmups']:
            if getattr(args, field) is not None:
                settings[field] = getattr(args, field)
        if not 1 <= settings['repetitions'] <= 100 or not 0 <= settings['warmups'] <= 10:
            raise ValueError('repetitions: 1..100; warmups: 0..10')
        if args.payload_mib is not None:
            if not 1 <= args.payload_mib <= 256:
                raise ValueError('payload-mib: 1..256')
            settings['payload_bytes'] = args.payload_mib * 1024 * 1024
        if args.build:
            subprocess.run(['cargo', 'build', '--release', '--locked'], cwd=ROOT, check=True)
        if not args.config and not shutil.which('fc-match'):
            raise ValueError('Falta fc-match (Fontconfig).')
        config, omissions = (load_config(args.config), []) if args.config else discover(args.aura, args.font, allow_missing_aura=args.list)
        identities = adapters.identify_all(config['terminal']) if args.list else preflight(config)
        print('Emuladores: ' + ', '.join(f'{name} ({v.get("version", v["status"])})' for name, v in identities.items()))
        print(f'Fuente: {config["font_family"]} · {config["font_file"]}')
        for item in omissions:
            print(f'Omitido: {item["terminal"]} ({item["reason"]})')
        if args.list:
            return 0
        if args.current_session:
            args.display = os.environ.get('DISPLAY')
            if not args.display:
                raise ValueError('--current-session necesita DISPLAY accesible (X11 o XWayland). Ejecuta desde tu escritorio; Wayland nativo aún no está soportado.')
        dependencies(not args.display, args.nvim)
        if args.nvim and (config['columns'] < 12 or config['rows'] < 8):
            raise ValueError('La prueba Neovim necesita al menos 12 columnas y 8 filas.')
        nvim = adapters.identify(dict(binary='nvim', kind='aura')) if args.nvim else None
        if nvim and nvim['status'] != 'available':
            raise ValueError('Neovim no se puede ejecutar: ' + nvim.get('error', ''))
        base = Path(os.environ.get('XDG_STATE_HOME', str(Path.home() / '.local/state'))) / 'aura/benchmarks'
        session = outside_checkout(args.output or (base / (datetime.now(timezone.utc).strftime('%Y%m%d-%H%M%S') + '-' + uuid.uuid4().hex[:8])))
        session.mkdir(parents=True, mode=0o700, exist_ok=False)
        write_config(session / 'comparison.toml', config)
        # Apply the same schema validation to generated and hand-written config.
        config = load_config(session / 'comparison.toml')
        runner.save(session / 'launcher.json', dict(arguments=vars(args) | {'output': str(session), 'config': str(args.config) if args.config else None, 'report': None}, omissions=omissions, identities=identities))
        print(f'\nResultados privados: {session}\nPerfil {profile}: {settings["repetitions"]} repeticiones + {settings["warmups"]} calentamientos.', flush=True)
        if not args.display:
            print('Xvfb propio: pantalla virtual; consulta los renderers detectados en el informe.', flush=True)
        else:
            print(f'Usando X11 {args.display}; abre ventanas de prueba e inyecta teclas. No interactúes durante la campaña; perder el foco falla la prueba.', flush=True)
        manifest = runner.run(config, settings, session / 'native', display=args.display, xvfb=not args.display,
                              environment='unknown', geometry_policy=args.geometry_policy)
        report.render(session / 'native/run.json', session / 'native/report')
        success = manifest['status'] == 'complete'
        if args.nvim and success:
            app = daily.run(session / 'native/run.json', session / 'neovim', nvim,
                            display=args.display, xvfb=not args.display)
            success = app['status'] == 'complete'
        show_report(session, args.no_open)
        return 0 if success else 1
    except (OSError, ValueError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
        print(f'benchmark: {error}', file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print('\nBenchmark interrumpido; los datos ya escritos se conservan. Usa --report CARPETA para verlos.', file=sys.stderr)
        return 130


if __name__ == '__main__':
    raise SystemExit(main())
