#!/bin/sh
# Prueba de punta a punta de la cadena de agentes, con los binarios reales y
# un Codex de mentira (eval/e2e/bin/codex) que no gasta cuota: EVA despacha la
# tarea, inyecta su servidor MCP, el agente llama herramientas reales por el
# socket privado con token, el gateway las juzga y las audita.
#
# Uso: eval/e2e-agent.sh            # binarios de target/debug (los compila)
#      eval/e2e-agent.sh --app      # los de target/EVA01.app (packaging/build-app.sh)
#
# Corre en un HOME temporal: no toca tu historial, tu configuración ni tus
# proyectos. Muestra una notificación de prueba («puedes ignorarla»).

set -eu
REPO="$(cd "$(dirname "$0")/.." && pwd)"
if [ "${1:-}" = "--app" ]; then
    BIN="$REPO/target/EVA01.app/Contents/MacOS"
else
    (cd "$REPO" && cargo build -q -p eva-worker -p eva-mcp -p eva-cli)
    BIN="$REPO/target/debug"
fi

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT
mkdir -p "$SCRATCH/home/Library/Application Support/EVA01" "$SCRATCH/proyecto"
# Sin anunciar el resultado en voz alta ni con otra notificación: la prueba
# ya verifica la herramienta notify, que es la que importa.
cat > "$SCRATCH/home/Library/Application Support/EVA01/config.toml" <<'TOML'
[feedback]
speak_task_results = false
notify_task_results = false
TOML

cd "$SCRATCH/proyecto"
OUT="$(HOME="$SCRATCH/home" PATH="$REPO/eval/e2e/bin:$PATH" \
    "$BIN/eva" intent --run --timeout-secs 120 "Adán, usa codex y prueba las herramientas de EVA" 2>&1)"
AUDIT="$(HOME="$SCRATCH/home" "$BIN/eva" audit 2>&1)"

fail=0
expect() { # texto, dónde, qué significa
    if printf '%s' "$2" | grep -qF -- "$1"; then
        echo "✓ $3"
    else
        echo "✗ $3 (no apareció «$1»)"
        fail=1
    fi
}
expect '"provider":"codex"' "$OUT" "la voz se interpretó como tarea para Codex"
expect "Codex empezó" "$OUT" "el worker lanzó el agente"
expect "herramientas: ask_user_confirmation,close_app,get_active_window,get_selection,insert_text,list_projects,notify,open_app,open_url,speak" \
    "$OUT" "eva-mcp sirvió las 10 herramientas al agente"
expect "notify=ok" "$OUT" "una herramienta permitida llegó y se ejecutó"
expect "close_app=ERROR: Rechazado" "$OUT" "cerrar Finder fue rechazado y el agente lo supo"
expect "✓ terminó" "$OUT" "la tarea terminó bien"
expect "permitido   agente  notify" "$AUDIT" "la auditoría registra la notificación del agente"
expect "bloqueado   agente  close_app Finder" "$AUDIT" "la auditoría registra el bloqueo"

if [ "$fail" -ne 0 ]; then
    echo
    echo "--- salida de eva ---"
    printf '%s\n' "$OUT"
    echo "--- auditoría ---"
    printf '%s\n' "$AUDIT"
    exit 1
fi
echo "Cadena de agentes: OK"
