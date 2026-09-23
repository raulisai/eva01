#!/bin/sh
# Assembles EVA01.app: builds eva-shell, eva-worker, eva-mcp and eva (the CLI)
# in release mode, lays them out as a real macOS app bundle with the
# Info.plist next to them, and signs it — the missing piece identified when reviewing what
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

# The version is the workspace's, so the bundle, `eva --version` and the
# release tag cannot drift apart.
VERSION="$(awk -F'"' '/^version *=/ {print $2; exit}' "$REPO_ROOT/Cargo.toml")"

echo "==> Compilando eva-shell, eva-worker, eva-mcp y eva ($VERSION) en modo release"
(cd "$REPO_ROOT" && cargo build --release -p eva-shell -p eva-worker -p eva-mcp -p eva-cli)

echo "==> Armando $APP_NAME"
rm -rf "$APP_PATH"
mkdir -p "$APP_PATH/Contents/MacOS"
mkdir -p "$APP_PATH/Contents/Frameworks"
# All four live side by side: the shell finds the worker, the worker finds
# eva-mcp (to hand to the agents), and `eva startup enable` finds the bundle,
# each by looking next to its own executable.
for binary in eva-shell eva-worker eva-mcp eva; do
    cp "$REPO_ROOT/target/release/$binary" "$APP_PATH/Contents/MacOS/$binary"
done
cp "$REPO_ROOT/packaging/Info.plist" "$APP_PATH/Contents/Info.plist"
mkdir -p "$APP_PATH/Contents/Resources"
cp "$REPO_ROOT/LICENSE" "$REPO_ROOT/THIRD_PARTY_NOTICES.md" "$APP_PATH/Contents/Resources/"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP_PATH/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" "$APP_PATH/Contents/Info.plist"

# `eva-worker` links `libeva_formatter.dylib` (the Apple Intelligence bridge
# built by `crates/eva-text/build.rs`) with an absolute `-install_name`
# pointing at that crate's build-time OUT_DIR — correct for `cargo run`
# during development, but meaningless once this .app is moved to another
# machine or that OUT_DIR is cleaned. Embed the dylib in the bundle's own
# Frameworks/ and repoint both the dylib's own id and eva-worker's reference
# to it at `@executable_path`-relative paths, which resolve correctly
# wherever the .app itself is copied.
# Cargo leaves old build-script output directories behind across
# incremental rebuilds (a new one appears whenever eva-text's own fingerprint
# changes) without cleaning up the previous ones — found for real: this repo
# already had two `eva-text-<hash>/` directories under target/release/build
# by the time this script was first written. Picking the first `find` match
# would silently package a stale dylib (an old prompt, an old bug fix) with
# no error and no visible sign anything was wrong, so this explicitly sorts
# by modification time and takes the newest.
DYLIB_SRC="$(
    find "$REPO_ROOT/target/release/build" -maxdepth 3 -iname "libeva_formatter.dylib" -exec stat -f "%m %N" {} \; 2>/dev/null \
        | sort -rn | head -1 | cut -d' ' -f2-
)"
if [ -z "$DYLIB_SRC" ]; then
    echo "no se encontró libeva_formatter.dylib bajo target/release/build — ¿falló build.rs de eva-text?" >&2
    exit 1
fi
DYLIB_DEST="$APP_PATH/Contents/Frameworks/libeva_formatter.dylib"
cp "$DYLIB_SRC" "$DYLIB_DEST"

OLD_DYLIB_PATH="$(otool -L "$APP_PATH/Contents/MacOS/eva-worker" | awk '/libeva_formatter\.dylib/ {print $1}')"
if [ -z "$OLD_DYLIB_PATH" ]; then
    echo "eva-worker no referencia libeva_formatter.dylib — ¿se compiló sin el bridge de Apple Intelligence?" >&2
    exit 1
fi
install_name_tool -id "@executable_path/../Frameworks/libeva_formatter.dylib" "$DYLIB_DEST"
install_name_tool -change "$OLD_DYLIB_PATH" "@executable_path/../Frameworks/libeva_formatter.dylib" \
    "$APP_PATH/Contents/MacOS/eva-worker"

echo "==> Firmando con identidad: $SIGNING_IDENTITY"
# El Hardened Runtime (--options runtime) activa Library Validation, que
# exige que todo dylib cargado tenga el mismo Team ID que el ejecutable
# principal. Una identidad ad-hoc ("-") no lleva Team ID real — encontrado
# al probar de verdad: con --options runtime y firma ad-hoc, dyld rechazaba
# cargar libeva_formatter.dylib con "different Team IDs" aunque ambos
# binarios se firmaran en el mismo `codesign --deep`, porque cada firma
# ad-hoc es su propia identidad sin Team ID compartido. El Hardened Runtime
# solo importa para notarizar con un Developer ID real (docs/PLAN.md §10
# decisión 7) — con firma ad-hoc, que es solo para probar en esta Mac, se
# omite para que el bridge de Apple Intelligence pueda cargar.
CODESIGN_EXTRA_OPTS=""
if [ "$SIGNING_IDENTITY" != "-" ]; then
    CODESIGN_EXTRA_OPTS="--options runtime"
fi
# shellcheck disable=SC2086 # word-splitting is intentional here: an empty flag must vanish, not become an empty argument
codesign --force --deep $CODESIGN_EXTRA_OPTS \
    --identifier "dev.eva01.app" \
    --sign "$SIGNING_IDENTITY" \
    "$APP_PATH"

echo "==> Verificando la firma"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"
codesign -dv "$APP_PATH"

echo
echo "Listo: $APP_PATH"
echo "Ejecutar con: open \"$APP_PATH\""
echo "La CLI queda en $APP_PATH/Contents/MacOS/eva (prueba: eva doctor)."
echo "La primera vez macOS pedirá Micrófono; EVA01 abrirá por sí sola el aviso de"
echo "Accesibilidad (Ajustes → Privacidad y seguridad → Accesibilidad), que necesita"
echo "para la tecla fn, pegar con Cmd+V y leer el texto seleccionado; sin él EVA01 no puede"
echo "hacer ninguna de las tres."
