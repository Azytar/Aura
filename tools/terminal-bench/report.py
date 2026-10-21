"""Private self-contained comparison artifacts, with missing data kept explicit."""

import csv
import html
import json
import os
from urllib.parse import quote
from common import outside_checkout, summarize

METRICS = {
    "startup_ready_ms": ("Inicio hasta PTY preparado", "ms", False),
    "startup_observed_pixel_ms": (
        "Inicio → testigo observado en X11 (cota)",
        "ms",
        False,
    ),
    "ascii_mib_s": ("ASCII procesado", "MiB/s", True),
    "unicode_mib_s": ("Unicode procesado", "MiB/s", True),
    "sgr_mib_s": ("Texto con SGR procesado", "MiB/s", True),
    "tui_mib_s": ("Actualizaciones TUI procesadas", "MiB/s", True),
    "key_to_child_ms": ("Tecla → proceso del PTY", "ms", False),
    "key_to_child_under_output_ms": ("Tecla → PTY con salida continua", "ms", False),
    "key_to_observed_pixel_ms": ("Tecla → píxel observado en X11 (cota)", "ms", False),
    "resize_ack_ms": ("Resize → geometría del PTY", "ms", False),
    "idle_cpu_percent": ("CPU en reposo", "% de un núcleo", False),
    "idle_rss_mib": ("RSS muestreado en reposo", "MiB", False),
    "load_cpu_seconds": ("CPU de las fases de carga", "s de CPU", False),
    "sampled_peak_rss_mib": ("Pico RSS muestreado tras preparar el PTY", "MiB", False),
}
COLORS = {
    "aura": "#3b82f6",
    "alacritty": "#ef4444",
    "kitty": "#f59e0b",
    "wezterm": "#a78bfa",
    "xterm": "#64748b",
}


def number(value):
    return "—" if value is None else f"{value:.3f}"


def cohort(row, policy):
    return (
        "equal-grid"
        if policy == "grid"
        else "cell-" + "x".join(map(str, row.get("cell_pixels", [])))
    )


def aggregate(manifest, baseline=None):
    if manifest.get("schema") != 1:
        raise ValueError("unsupported run schema")
    trials = manifest["trials"]
    policy = manifest["geometry_policy"]
    if policy not in ("cell", "grid"):
        raise ValueError("unsupported geometry policy")
    groups = sorted(
        {
            cohort(row, policy)
            for row in trials
            if row["status"] == "ok" and not row["warmup"]
        }
    ) or ["unavailable"]
    rows = []
    for group in groups:
        for metric in METRICS:
            for entry in manifest["config"]["terminal"]:
                name = entry["id"]
                all_trials = [row for row in trials if row["terminal"] == name]
                measured = [row for row in all_trials if not row["warmup"]]
                valid = [
                    row
                    for row in measured
                    if row["status"] == "ok"
                    and cohort(row, policy) == group
                    and metric in row["metrics"]
                ]
                values = [value for row in valid for value in row["metrics"][metric]]
                failed = sum(row["status"] != "ok" for row in all_trials)
                status = "complete"
                if manifest["terminal_identities"][name]["status"] != "available":
                    status = "unavailable"
                elif not any(
                    row["status"] == "ok" and cohort(row, policy) == group
                    for row in measured
                ) and any(row["status"] == "ok" for row in measured):
                    status = "different-geometry"
                elif not values:
                    status = "missing-metric"
                elif failed or len(valid) != manifest["settings"]["repetitions"]:
                    status = "partial" if values else "failed"
                rows.append(
                    dict(
                        terminal=name,
                        kind=entry["kind"],
                        cohort=group,
                        metric=metric,
                        unit=METRICS[metric][1],
                        better="higher" if METRICS[metric][2] else "lower",
                        status=status,
                        trials=len(valid),
                        failed_trials=failed,
                        **summarize(values),
                    )
                )
    for row in rows:
        reference = next(
            (
                v
                for v in rows
                if v["terminal"] == baseline
                and v["metric"] == row["metric"]
                and v["cohort"] == row["cohort"]
            ),
            None,
        )
        row["delta_percent"] = None
        if (
            row["status"] == "complete"
            and reference
            and reference["status"] == "complete"
            and reference["p50"]
        ):
            row["delta_percent"] = 100 * (row["p50"] / reference["p50"] - 1)
    return rows


def svg_chart(rows, title, unit, higher):
    width, height = max(420, len(rows) * 110 + 70), 300
    maximum = max((row["max"] or 0 for row in rows), default=0) or 1
    floor, plot = 232, 160
    pieces = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" role="img">',
        f"<title>{html.escape(title)} ({html.escape(unit)}); barras=p50, líneas=mín–máx observado</title>",
        f'<rect width="{width}" height="{height}" fill="#101827"/>',
        f'<text x="20" y="24" fill="#f1f5f9" font-family="sans-serif" font-size="14">{html.escape(title)}</text>',
        f'<text x="20" y="45" fill="#94a3b8" font-family="sans-serif" font-size="11">{html.escape(unit)} · {"↑ mayor" if higher else "↓ menor"} es mejor · p50 y rango observado</text>',
        f'<line x1="24" y1="{floor}" x2="{width - 15}" y2="{floor}" stroke="#64748b"/>',
    ]
    span = (width - 60) / max(1, len(rows))
    for index, row in enumerate(rows):
        center = 35 + span * (index + 0.5)
        name = html.escape(row["terminal"], quote=True)
        pieces.append(f'<g data-terminal="{name}">')
        if row["p50"] is not None:
            y = floor - plot * row["p50"] / maximum
            low = floor - plot * row["min"] / maximum
            high = floor - plot * row["max"] / maximum
            color = (
                COLORS.get(row["kind"], "#64748b")
                if row["status"] == "complete"
                else "#a16207"
            )
            pieces.extend(
                [
                    f'<rect x="{center - 25:.2f}" y="{y:.2f}" width="50" height="{floor - y:.2f}" fill="{color}"><title>{name}: p50={number(row["p50"])}, n={row["n"]}, {row["status"]}</title></rect>',
                    f'<path d="M{center:.2f},{low:.2f} V{high:.2f} M{center - 5:.2f},{low:.2f} H{center + 5:.2f} M{center - 5:.2f},{high:.2f} H{center + 5:.2f}" stroke="#e2e8f0" fill="none"/>',
                    f'<text x="{center:.2f}" y="{max(65, y - 7):.2f}" text-anchor="middle" fill="#f1f5f9" font-family="sans-serif" font-size="12">{number(row["p50"])}</text>',
                ]
            )
        else:
            pieces.append(
                f'<text x="{center:.2f}" y="200" text-anchor="middle" fill="#94a3b8" font-family="sans-serif" font-size="11">sin datos</text>'
            )
        pieces.extend(
            [
                f'<text x="{center:.2f}" y="252" text-anchor="middle" fill="#e2e8f0" font-family="sans-serif" font-size="11"><title>{name}</title>{html.escape(row["terminal"][:15])}</text>',
                f'<text x="{center:.2f}" y="272" text-anchor="middle" fill="#94a3b8" font-family="sans-serif" font-size="10">{html.escape(row["status"])}</text>',
                "</g>",
            ]
        )
    pieces.append("</svg>")
    return "\n".join(pieces)


def render(source, output, baseline=None):
    source = outside_checkout(source)
    output = outside_checkout(output)
    manifest = json.loads(source.read_text())
    if baseline is not None and baseline not in {
        entry["id"] for entry in manifest["config"]["terminal"]
    }:
        raise ValueError("baseline is not a configured terminal ID")
    rows = aggregate(manifest, baseline)
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    summary = dict(
        schema=1,
        source_sha256=__import__("hashlib").sha256(source.read_bytes()).hexdigest(),
        baseline=baseline,
        rows=rows,
    )
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    fields = list(rows[0]) if rows else []
    with (output / "summary.csv").open("w", newline="") as destination:
        writer = csv.DictWriter(destination, fieldnames=fields)
        writer.writeheader()
        writer.writerows(rows)
    groups = sorted({row["cohort"] for row in rows})
    selected_group = (
        max(
            groups,
            key=lambda group: len(
                {
                    row["terminal"]
                    for row in rows
                    if row["cohort"] == group and row["status"] == "complete"
                }
            ),
        )
        if groups
        else ""
    )
    group_options = '<option value="">Todas las geometrías</option>' + "".join(
        f'<option value="{html.escape(group, quote=True)}" {"selected" if group == selected_group else ""}>{html.escape(group)}</option>'
        for group in groups
    )
    controls = "".join(
        f'<label><input type="checkbox" checked value="{html.escape(entry["id"], quote=True)}"> {html.escape(entry["id"])}</label> '
        for entry in manifest["config"]["terminal"]
    )
    environment = manifest.get("environment", {})
    software = any(
        environment.get(name, {}).get("software_mentions") for name in ("gl", "vulkan")
    )
    warning = (
        "Xvfb / software: prueba funcional, no clasificación de GPU física."
        if environment.get("xvfb")
        else f"Renderizado declarado: {environment.get('declared_rendering', 'unknown')}; GPU por emulador sin certificar."
    )
    if software:
        warning += " El inventario contiene un renderer por software."
    geometry = (
        "Solo se comparan celdas con las mismas dimensiones de píxel."
        if manifest["geometry_policy"] == "cell"
        else "Comparación por rejilla: los tamaños de píxel pueden diferir entre emuladores."
    )
    cards = []
    for index, (group, metric) in enumerate(
        dict.fromkeys((row["cohort"], row["metric"]) for row in rows)
    ):
        selected = [
            row for row in rows if row["cohort"] == group and row["metric"] == metric
        ]
        title, unit, higher = METRICS[metric]
        svg = svg_chart(selected, title, unit, higher)
        name = f"chart-{index:02}.svg"
        (output / name).write_text(svg)
        cards.append(
            f'<section data-cohort="{html.escape(group, quote=True)}"><h2>{html.escape(group)}</h2>{svg}<a href="{name}">SVG</a></section>'
        )
    table = []
    for row in rows:
        title, unit, _ = METRICS[row["metric"]]
        cells = [
            row["terminal"],
            title + " (" + unit + ")",
            row["cohort"],
            row["status"],
            str(row["trials"]),
            str(row["n"]),
            number(row["p50"]),
            number(row["p95"]),
            number(row["p99"]),
            number(row["min"]) + "–" + number(row["max"]),
            number(row["delta_percent"]),
        ]
        table.append(
            f'<tr data-terminal="{html.escape(row["terminal"], quote=True)}" data-cohort="{html.escape(row["cohort"], quote=True)}">'
            + "".join(f"<td>{html.escape(value)}</td>" for value in cells)
            + "</tr>"
        )
    failures = [
        f"{row['terminal']}, ronda {row['round']}, warmup={row['warmup']}: {row.get('error', 'failed')}"
        for row in manifest["trials"]
        if row["status"] != "ok"
    ]
    if manifest.get("active_trial"):
        failures.append(
            "Prueba incompleta: "
            + json.dumps(manifest["active_trial"], ensure_ascii=False)
        )
    identities = "\n".join(
        f"{name}: {value.get('version', value.get('error', 'unavailable'))}; SHA256={value.get('sha256', '—')}"
        for name, value in manifest["terminal_identities"].items()
    )
    source_link = quote(os.path.relpath(source, output))
    document = """<!doctype html><html lang="es"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Comparación de emuladores</title><style>
body{font:15px system-ui,sans-serif;background:#080f1c;color:#e2e8f0;margin:24px;line-height:1.5}a{color:#93c5fd}
h1{margin-bottom:4px}.notice{background:#1e293b;border-left:4px solid #f59e0b;padding:16px}label{display:inline-block;margin:8px}
.charts{display:grid;grid-template-columns:repeat(auto-fit,minmax(370px,1fr));gap:18px}section{background:#101827;border:1px solid #263347;border-radius:8px;padding:10px}
section h2{font-size:13px;color:#94a3b8;margin:0}svg{display:block;width:100%}.table{overflow:auto}table{border-collapse:collapse;white-space:nowrap}th,td{padding:8px;border-bottom:1px solid #334155;text-align:right}td:first-child,td:nth-child(2){text-align:left}pre{white-space:pre-wrap;overflow-wrap:anywhere}small{color:#94a3b8}[hidden]{display:none!important}</style>"""
    document += f'''<h1>Comparación de emuladores</h1><p>{html.escape(manifest["created_utc"])} · estado: {html.escape(manifest["status"])}</p>
<div class="notice">{html.escape(warning)}<br>{html.escape(geometry)}<br>La observación X11 incluye captura y sondeo; no mide scanout ni input-to-photon.</div>
<p>Barras: p50; líneas: mínimo–máximo observado, sin intervalo de confianza. p95/p99 usan nearest rank.
Los warmups y las pruebas fallidas no aportan muestras. Resultados parciales no generan comparación relativa.
CPU: árbol de procesos observado, sin helper ni servidor X; RSS suma páginas residentes, con páginas compartidas contadas por proceso y muestreo de 10 ms.</p>
<p><a href="summary.csv">CSV</a> · <a href="summary.json">Resumen JSON</a> · <a href="{source_link}">Repeticiones y procedencia</a></p>
<div id="filters"><label>Geometría <select id="cohort">{group_options}</select></label>{controls}</div><div class="charts">{"".join(cards)}</div><h2>Resultados detallados</h2>
<p>Delta = cambio de p50 respecto a {html.escape(baseline or "ninguna referencia")} en la misma cohorte; el signo conserva las unidades originales.</p>
<div class="table"><table><thead><tr>{"".join("<th>" + v + "</th>" for v in ["Emulador", "Métrica", "Cohorte", "Estado", "Pruebas", "Muestras", "p50", "p95", "p99", "mín–máx", "Delta %"])}</tr></thead><tbody>{"".join(table)}</tbody></table></div>
<details><summary>Fallos y omisiones</summary><pre>{html.escape(chr(10).join(failures) or "Sin pruebas fallidas; consulte las identidades para ejecutables ausentes.")}</pre></details>
<details><summary>Versiones y hashes</summary><pre>{html.escape(identities)}</pre></details>
<script>
function filter(){{
const group=document.getElementById('cohort').value;
const enabled=new Set(Array.from(document.querySelectorAll('#filters input:checked'),v=>v.value));
document.querySelectorAll('[data-cohort]').forEach(row=>row.hidden=!!group&&row.dataset.cohort!==group);
document.querySelectorAll('[data-terminal]').forEach(row=>row.style.display=enabled.has(row.dataset.terminal)?'':'none');
}}
document.querySelectorAll('#filters input,#cohort').forEach(input=>input.addEventListener('change',filter));filter();
</script></html>'''
    (output / "index.html").write_text(document)
    return summary
