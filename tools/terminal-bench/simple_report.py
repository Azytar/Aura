"""Linear median bars for the terminal and browser; detailed evidence stays available."""
import csv
import html
import json
import sys

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
    elements = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} 280" role="img">',
                f'<title>{html.escape(title)}; mediana en {html.escape(unit)}</title>',
                f'<rect width="{width}" height="280" fill="#101827"/>',
                f'<text x="16" y="23" fill="#e2e8f0" font-family="sans-serif" font-size="14">{html.escape(title)}</text>',
                f'<text x="16" y="45" fill="#9fb2cd" font-family="sans-serif" font-size="12">{html.escape(unit)} · {"↑ Más" if higher else "↓ Menos"} es mejor</text>',
                f'<line x1="16" y1="230" x2="{width-16}" y2="230" stroke="#40506b"/>']
    for index, row in enumerate(rows):
        center = 16 + (width - 32) * (index + .5) / len(rows)
        name = html.escape(row['terminal'], quote=True)
        elements.append(f'<g data-terminal="{name}">')
        if row['p50'] is not None:
            height = row['p50'] / maximum * 150
            color = report.COLORS.get(row['kind'], '#9aaec5') if row['status'] == 'complete' else '#a16207'
            elements += [f'<rect x="{center-30}" y="{230-height:.3f}" width="60" height="{height:.3f}" fill="{color}"/>',
                         f'<text x="{center}" y="{219-height:.3f}" text-anchor="middle" fill="#f1f5f9" font-family="sans-serif" font-size="17">{report.number(row["p50"])}</text>']
        else:
            elements.append(f'<text x="{center}" y="210" text-anchor="middle" fill="#94a3b8" font-family="sans-serif" font-size="12">sin datos</text>')
        elements.append(f'<text x="{center}" y="254" text-anchor="middle" fill="#e2e8f0" font-family="sans-serif" font-size="13"><title>{name}</title>{html.escape(row["terminal"][:15])}</text></g>')
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
    cards = []
    order = list(dict.fromkeys(FEATURED + list(metrics)))
    for group in groups:
        for metric in order:
            selected = [r for r in rows if r['cohort'] == group and r['metric'] == metric]
            if not selected:
                continue
            title, unit, higher = metrics[metric]
            filename = f'chart-{len(cards):02}.svg'
            (output / filename).write_text(svg(selected, title, unit, higher))
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
body{font:15px system-ui,sans-serif;background:#080f1c;color:#e2e8f0;max-width:1200px;margin:30px auto;padding:0 20px;line-height:1.6}a,summary{color:#91c4ff}summary{cursor:pointer}.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,450px),1fr));gap:20px}section{padding:18px;background:#101827;border:1px solid #28364e;border-radius:12px}img{width:100%;display:block}small{color:#9fb2cd}pre{white-space:pre-wrap;overflow-wrap:anywhere}table{border-collapse:collapse;font-size:12px}td,th{padding:7px;text-align:right;border-bottom:1px solid #30405a}.table{overflow:auto}footer{margin:25px 0}</style><h1>Aura frente a otros terminales</h1><p>Una barra por emulador y un valor: la mediana. Escalas lineales desde cero; detalles debajo de cada gráfica.</p>'''
    environment = native.get('environment', {})
    if environment.get('xvfb') or any(environment.get(k, {}).get('software_mentions') for k in ['gl', 'vulkan']):
        document += '<p>Xvfb / renderizado por software: estos resultados no clasifican GPU físicas.</p>'
    document += f'<p>{html.escape(geometry)} Observación X11, no input-to-photon. Estado: {html.escape(", ".join(failures))}.</p>'
    document += '<details><summary>Qué miden estas pruebas</summary><p>La salida mide bytes procesados hasta una respuesta ordenada del terminal, no frames presentados. CPU y RSS cubren el árbol del terminal tras preparar el PTY, excluyendo el helper y el servidor X. RSS es un muestreo, no PSS, VRAM ni pico instantáneo garantizado. El informe completo conserva método, geometría, versiones, fallos y todas las repeticiones.</p></details>'
    if daily:
        document += '<p>Neovim real instrumentado: buffer verificado y testigo en la barra de estado; incluye el coste del editor, callback y control. Paginado y búsqueda se verifican fuera del reloj de escritura. Sin plugins/LSP personales.</p>'
    document += '<div class="cards">' + ''.join(cards) + '</div><footer><a href="../native/report/index.html">Informe nativo completo, fallos y procedencia</a> · <a href="summary.json">Estadísticas y hashes de origen</a> · <a href="summary.csv">CSV</a>'
    if daily:
        document += ' · <a href="../neovim/run.json">Repeticiones Neovim</a>'
    document += '<details><summary>Versiones</summary><pre>' + html.escape(versions) + '</pre></details></footer></html>'
    (output / 'index.html').write_text(document)
    (output / 'summary.json').write_text(json.dumps(dict(rows=rows, sources={str(p): sha256(p) for p in sources}), indent=2))
    with (output / 'summary.csv').open('w', newline='') as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    console(rows, metrics)
    return output / 'index.html'
