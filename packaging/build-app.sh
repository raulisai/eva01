#!/bin/sh
# Assembles EVA01.app: builds eva-shell and eva-worker in release mode,
# lays them out as a real macOS app bundle with the Info.plist next to
# them, and signs it — the missing piece identified when reviewing what
# blocks testing the MVP end to end (docs/PLAN.md §3.3/§10 decision 7):
# an unbundled binary has no Info.plist to attribute a microphone-usage
# prompt to, and a signature that changes every rebuild cannot keep a
# permission grant across builds.
#
# Usage:
#   packaging/build-app.sh                      # ad-hoc signed, for local testing
#   SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
#     packaging/build-app.sh                    # real signature, for anything you'll actually distribute
#
# What this does NOT solve, stated plainly: ad-hoc signing (the default
# here) has no stable identity tied to a certificate, so macOS may still
# reset the permission grant on some rebuilds — that is exactly why
# docs/PLAN.md §10 decision 7 leaves "Apple Developer ID cuando quieras
# distribuir" as a real, separate, paid step ($99/año), not something a
# shell script can substitute for.

set -eu

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="EVA01.app"
APP_PATH="$REPO_ROOT/target/$APP_NAME"
SIGNING_IDENTITY="${SIGNING_IDENTITY:--}" # "-" is codesign's ad-hoc identity

echo "==> Compilando eva-shell y eva-worker en modo release"
(cd "$REPO_ROOT" && cargo build --release -p eva-shell -p eva-worker)

echo "==> Armando $APP_NAME"
rm -rf "$APP_PATH"
mkdir -p "$APP_PATH/Contents/MacOS"
cp "$REPO_ROOT/target/release/eva-shell" "$APP_PATH/Contents/MacOS/eva-shell"
cp "$REPO_ROOT/target/release/eva-worker" "$APP_PATH/Contents/MacOS/eva-worker"
cp "$REPO_ROOT/packaging/Info.plist" "$APP_PATH/Contents/Info.plist"

echo "==> Firmando con identidad: $SIGNING_IDENTITY"
codesign --force --deep --options runtime \
    --identifier "dev.eva01.app" \
    --sign "$SIGNING_IDENTITY" \
    "$APP_PATH"

echo "==> Verificando la firma"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"
codesign -dv "$APP_PATH"

echo
echo "Listo: $APP_PATH"
echo "Ejecutar con: open \"$APP_PATH\""
echo "(la primera vez, macOS pedirá permiso de Micrófono y, para Cmd+V simulado,"
echo " hay que habilitar EVA01 a mano en Ajustes → Privacidad y seguridad → Accesibilidad —"
echo " CGEventPost no dispara ese diálogo por sí solo; ver el TODO en eva-macos::paste)"
