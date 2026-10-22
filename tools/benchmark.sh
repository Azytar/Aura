#!/usr/bin/env bash
# One-command entry point; all measurements and generated configuration stay private.
set -euo pipefail
umask 077

if ! command -v python3 >/dev/null 2>&1; then
    printf '%s\n' 'Falta Python 3.11 o posterior. Instálalo y vuelve a ejecutar este script.' >&2
    exit 1
fi
if ! python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))'; then
    printf '%s\n' 'Se necesita Python 3.11 o posterior.' >&2
    exit 1
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec python3 -B "$script_dir/terminal-bench/quickstart.py" "$@"
