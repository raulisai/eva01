#!/bin/sh
# Regenera packaging/AppIcon.icns desde el dibujo vectorial de make-icon.swift.
# El .icns está versionado: build-app.sh solo lo copia, no hace falta correr esto
# salvo que cambies el diseño.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
ICONSET="$(mktemp -d)/AppIcon.iconset"
swift "$HERE/make-icon.swift" "$ICONSET"
iconutil -c icns "$ICONSET" -o "$HERE/AppIcon.icns"
echo "escrito $HERE/AppIcon.icns"
