"""Linear median bars for the terminal and browser; detailed evidence stays available."""
import csv
import html
import json
import math
import sys
import textwrap

from common import outside_checkout, sha256
import report

DAILY_METRICS = {
    'nvim_edit_continuous_ms': ('Escribir seguido en Neovim', 'ms', False),
    'nvim_edit_after_pause_ms': ('Escribir tras una pausa en Neovim', 'ms', False),
    'nvim_startup_ms': ('Inicio de terminal y Neovim observado', 'ms', False),
}
FEATURED = [*DAILY_METRICS, 'key_to_child_under_output_ms', 'unicode_mib_s',
            'idle_rss_mib', 'resize_ack_ms']


def svg(rows, title, unit, higher):
    width = max(480, len(rows) * 116 + 16)
    maximum = max((r['p50'] or 0 for r in rows), default=0) or 1
    elements = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} 310" role="img">',
                f'<title>{html.escape(title)}; mediana en {html.escape(unit)}</title>',
                f'<rect width="{width}" height="310" fill="#ffffff"/>',
                f'<text x="16" y="20" fill="#666666" font-family="sans-serif" font-size="12">Mediana · {html.escape(unit)} · {"↑ Más" if higher else "↓ Menos"} es mejor</text>',
                f'<line x1="16" y1="220" x2="{width-16}" y2="220" stroke="#555555"/>']
    cohorts = sorted({r['cohort'] for r in rows if 'cohort' in r})
    if cohorts:
        elements.append(f'<text x="16" y="35" fill="#666666" font-family="sans-serif" font-size="11">{html.escape(", ".join(cohorts))}</text>')
    for index, row in enumerate(rows):
        center = 16 + (width - 32) * (index + .5) / len(rows)
        name = html.escape(row['terminal'], quote=True)
        elements.append(f'<g data-terminal="{name}">')
        if row['p50'] is not None:
            height = row['p50'] / maximum * 150
            color = '#4169ff' if row['kind'] == 'aura' else '#e7e7e7'
            status = '' if row['status'] == 'complete' else ' *'
            elements += [f'<rect x="{center-30}" y="{220-height:.3f}" width="60" height="{height:.3f}" fill="{color}"/>',
                         f'<text x="{center}" y="{210-height:.3f}" text-anchor="middle" fill="#262626" font-family="sans-serif" font-size="17">{report.number(row["p50"])}{status}</text>']
        else:
            elements.append(f'<text x="{center}" y="200" text-anchor="middle" fill="#666666" font-family="sans-serif" font-size="12">sin datos</text>')
        elements.append(f'<text x="{center}" y="241" text-anchor="middle" fill="#262626" font-family="sans-serif" font-size="13"><title>{name}; {html.escape(row["status"])}</title>{html.escape(row["terminal"][:15])}</text></g>')
    for index, line in enumerate(textwrap.wrap(title, width=48)[:2]):
        elements.append(f'<text x="{width/2}" y="{270+index*19}" text-anchor="middle" fill="#262626" font-family="sans-serif" font-size="16" font-weight="bold">{html.escape(line)}</text>')
    return '\n'.join(elements + ['</svg>'])


def console(rows, metrics, stream=None):
    stream = stream or sys.stdout
    for cohort in sorted({r['cohort'] for r in rows}):
        print(f'\nBarras de medianas · {cohort} · escala lineal desde cero', file=stream)
        for metric in [m for m in FEATURED if m in metrics]:
            selected = [r for r in rows if r['metric'] == metric and r['cohort'] == cohort]
            if not any(r['p50'] is not None for r in selected):
                continue
            title, unit, higher = metrics[metric]
            print(f'\n{title} ({unit}; {"más" if higher else "menos"} es mejor)', file=stream)
            maximum = max((r['p50'] or 0 for r in selected), default=0) or 1
            for row in selected:
                if row['p50'] is None:
                    print(f'  {row["terminal"]:<16} sin datos ({row["status"]})', file=stream)
                else:
                    bar = '#' * round(30 * row['p50'] / maximum)
                    print(f'  {row["terminal"]:<16} {bar:<30} {row["p50"]:9.3f}  {row["status"]}', file=stream)


def render(native_path, output, daily_path=None, baseline=None):
    native_path = outside_checkout(native_path)
    native = json.loads(native_path.read_text())
    rows = report.aggregate(native, baseline)
    metrics = dict(report.METRICS)
    sources = [native_path]
    daily = None
    if daily_path:
        daily_path = outside_checkout(daily_path)
        daily = json.loads(daily_path.read_text())
        if native['config'] != daily['config'] or native['terminal_identities'] != daily['terminal_identities']:
            raise ValueError('Neovim and native runs use different configurations or binaries')
        rows += report.aggregate(daily, baseline, metrics=DAILY_METRICS)
        metrics.update(DAILY_METRICS)
        sources.append(daily_path)
    output = outside_checkout(output)
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    groups = sorted({r['cohort'] for r in rows})
    cards, overview = [], []
    order = list(dict.fromkeys(FEATURED + list(metrics)))
    for group in groups:
        for metric in order:
            selected = [r for r in rows if r['cohort'] == group and r['metric'] == metric]
            if not selected:
                continue
            title, unit, higher = metrics[metric]
            filename = f'chart-{len(cards):02}.svg'
            chart = svg(selected, title, unit, higher)
            (output / filename).write_text(chart)
            overview.append(chart)
            table = ''.join('<tr><td>' + html.escape(r['terminal']) + '</td><td>' + html.escape(r['status']) + '</td>' +
                            ''.join('<td>' + report.number(r[k]) + '</td>' for k in ['p50', 'p95', 'p99', 'min', 'max']) +
                            f'<td>{r["n"]}</td></tr>' for r in selected)
            cards.append(f'<section><small>{html.escape(group)}</small><img src="{filename}" alt="{html.escape(title)}">'
                         f'<details><summary>Detalles y percentiles</summary><div class="table"><table><tr><th>Terminal</th><th>Estado</th><th>Mediana</th><th>p95</th><th>p99</th><th>Mín.</th><th>Máx.</th><th>n</th></tr>{table}</table></div></details>'
                         f'<a href="{filename}" download>SVG</a></section>')
    versions = '\n'.join(f'{name}: {v.get("version", v.get("error", "unavailable"))}' for name, v in native['terminal_identities'].items())
    if daily:
        versions += '\nNeovim: ' + daily['nvim']['version']
    failures = [f'{path.name}: {json.loads(path.read_text())["status"]}' for path in sources]
    geometry = ('Se comparan celdas iguales.' if native['geometry_policy'] == 'cell' else
                'Misma rejilla; los tamaños de celda pueden diferir entre emuladores.')
    document = '''<!doctype html><html lang="es"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Comparación de terminales</title><style>
body{font:15px system-ui,sans-serif;background:#fff;color:#262626;max-width:1440px;margin:30px auto;padding:0 20px;line-height:1.6}a,summary{color:#2854c5}summary{cursor:pointer}.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,420px),1fr));gap:24px}section{padding:12px 0;border-bottom:1px solid #ddd}img{width:100%;display:block}small{color:#666}pre{white-space:pre-wrap;overflow-wrap:anywhere}table{border-collapse:collapse;font-size:12px}td,th{padding:7px;text-align:right;border-bottom:1px solid #ddd}.table{overflow:auto}footer{margin:25px 0}</style><h1>Aura frente a otros terminales</h1><p>Una barra por emulador y un valor: la mediana. Escalas lineales desde cero. Aura en azul; referencias en gris. * indica datos parciales; detalles debajo de cada gráfica.</p><p><a href="overview.svg">Todas las barras en una imagen SVG</a></p>'''
    environment = native.get('environment', {})
    document += '<p>' + html.escape(environment_label(environment)) + '</p>'
    profile = native['settings'].get('profile', 'unspecified')
    document += f'<p>Perfil: {html.escape(profile)}. quick es una comprobación funcional, insuficiente para un ranking estable.</p>'
    document += '<p>El inventario describe las sondas GL y los dispositivos Vulkan disponibles; no certifica el dispositivo elegido por cada emulador. xterm es una referencia Xlib.</p>'
    document += f'<p>{html.escape(geometry)} Observación X11, no input-to-photon. Estado: {html.escape(", ".join(failures))}.</p>'
    if native.get('window_policy'):
        document += '<p>Ventanas de prueba controladas fuera del mosaico y las decoraciones del gestor. Las métricas de inicio hasta el testigo incluyen la preparación de geometría; no representan el arranque habitual de una ventana gestionada.</p>'
    document += '<details><summary>Qué miden estas pruebas</summary><p>La salida mide bytes procesados hasta una respuesta ordenada del terminal, no frames presentados. CPU y RSS cubren el árbol del terminal tras preparar el PTY, excluyendo el helper y el servidor X. RSS es un muestreo, no PSS, VRAM ni pico instantáneo garantizado. El informe completo conserva método, geometría, versiones, fallos y todas las repeticiones.</p></details>'
    if daily:
        document += '<p>Neovim real instrumentado: buffer verificado y testigo en la barra de estado; incluye el coste del editor, callback y control. Paginado y búsqueda se verifican fuera del reloj de escritura. Sin plugins/LSP personales.</p>'
    document += '<div class="cards">' + ''.join(cards) + '</div><footer><a href="../native/report/index.html">Informe nativo completo, fallos y procedencia</a> · <a href="summary.json">Estadísticas y hashes de origen</a> · <a href="summary.csv">CSV</a>'
    if daily:
        document += ' · <a href="../neovim/run.json">Repeticiones Neovim</a>'
    document += '<details><summary>Versiones</summary><pre>' + html.escape(versions) + '</pre></details></footer></html>'
    (output / 'index.html').write_text(document)
    heading = f'<rect width="100%" height="100%" fill="white"/><text x="16" y="26" font-family="sans-serif" font-size="20">Comparación de terminales · {html.escape(profile)} · medianas · Aura azul / referencias gris</text>'
    caption = environment_label(environment) + ' quick: prueba funcional, no ranking. *: parcial.'
    if native.get('window_policy'):
        caption += ' Ventanas controladas fuera del mosaico; inicio hasta testigo incluye preparación.'
    lines = textwrap.wrap(caption, width=170)
    for index, line in enumerate(lines):
        heading += f'<text x="16" y="{49+index*17}" font-family="sans-serif" font-size="12">{html.escape(line)}</text>'
    header_height = max(100, 66 + 17 * len(lines))
    tiles = [chart.replace('<svg ', f'<svg x="{index%3*480}" y="{header_height+index//3*310}" width="480" height="310" ', 1)
             for index, chart in enumerate(overview)]
    (output / 'overview.svg').write_text(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1440 {header_height+max(1, math.ceil(len(overview)/3))*310}">' + heading + ''.join(tiles) + '</svg>')
    (output / 'summary.json').write_text(json.dumps(dict(rows=rows, sources={str(p): sha256(p) for p in sources}), indent=2))
    with (output / 'summary.csv').open('w', newline='') as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    console(rows, metrics)
    return output / 'index.html'


def environment_label(environment):
    mode = 'Xvfb propio / pantalla virtual' if environment.get('xvfb') else 'Sesión X11 / XWayland existente'
    descriptions = []
    for api in ['gl', 'vulkan']:
        probe = environment.get(api, {})
        classification = probe.get('classification', 'software' if probe.get('software_mentions') else 'unknown')
        descriptions.append(f'{api.upper()}: {classification} — ' + (', '.join(probe.get('renderers', [])) or 'renderer sin identificar'))
    return mode + '. ' + '; '.join(descriptions) + '. No mide presentación física.'
