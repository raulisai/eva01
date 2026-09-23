#!/bin/sh
# Regenera THIRD_PARTY_NOTICES.md: las dependencias de Cargo que entran en
# el binario de macOS (con su licencia) y la atribución de los modelos que
# `eva model install` descarga. Falla si alguna dependencia no declara
# licencia o es copyleft fuerte (GPL/AGPL/SSPL): eso no se distribuye por
# accidente.
#
# Uso: packaging/third-party.sh          # escribe THIRD_PARTY_NOTICES.md
#      packaging/third-party.sh --check  # falla si el archivo está desactualizado

set -eu
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$REPO_ROOT/THIRD_PARTY_NOTICES.md"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

cd "$REPO_ROOT"
cargo metadata --format-version 1 --locked --filter-platform aarch64-apple-darwin | python3 "$REPO_ROOT/packaging/third_party.py" > "$TMP"

if [ "${1:-}" = "--check" ]; then
    if ! cmp -s "$TMP" "$OUT"; then
        echo "THIRD_PARTY_NOTICES.md está desactualizado: corre packaging/third-party.sh" >&2
        exit 1
    fi
    echo "THIRD_PARTY_NOTICES.md al día"
else
    cp "$TMP" "$OUT"
    echo "escrito $OUT"
fi
