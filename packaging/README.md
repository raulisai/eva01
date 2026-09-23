# Empaquetado y release

## Armar el .app

```bash
sh packaging/build-app.sh                       # firma ad-hoc, para probar en esta Mac
SIGNING_IDENTITY="Developer ID Application: Tu Nombre (TEAMID)" \
  sh packaging/build-app.sh                     # firma real
```

Deja `target/EVA01.app` con `eva-shell` (el ejecutable principal), `eva-worker`, `eva-mcp`
y `eva` lado a lado en `Contents/MacOS/` (cada uno encuentra a los otros mirando junto a
sí mismo) y `libeva_formatter.dylib` (el puente de Apple Intelligence) en
`Contents/Frameworks/`. La versión sale de `Cargo.toml`.

El ícono (`AppIcon.icns`) sale de un dibujo vectorial (`make-icon.swift`); cámbialo y corre
`sh packaging/make-icon.sh` para regenerarlo.

`LICENSE` y `THIRD_PARTY_NOTICES.md` (dependencias y atribución de los modelos, que son CC BY
4.0) van en `Contents/Resources/`. Después de cambiar dependencias, regenera los avisos con
`sh packaging/third-party.sh` (CI corre `--check` y también rechaza dependencias sin licencia
o con copyleft fuerte).

**Firma ad-hoc vs. real.** Con ad-hoc no hay Hardened Runtime (macOS rechaza cargar el
dylib con «different Team IDs») y macOS puede olvidar los permisos de Micrófono y
Accesibilidad al recompilar. Con Developer ID el permiso se mantiene, y es lo único que
se puede notarizar.

## Release (CI)

`.github/workflows/release.yml` se dispara al empujar una etiqueta que coincida con la
versión de `Cargo.toml`:

```bash
# 1. sube la versión en Cargo.toml (0.2.0 → 0.3.0) y haz commit
git tag v0.3.0 && git push origin v0.3.0
```

Firma con Developer ID, notariza con `notarytool`, grapa el ticket y publica
`EVA01-vX.Y.Z.zip` con su `.sha256` en un GitHub Release.

Secretos del repositorio que necesita (Settings → Secrets and variables → Actions):

| Secreto | Qué es |
|---|---|
| `MACOS_CERTIFICATE` | el certificado «Developer ID Application» exportado como `.p12`, en base64 (`base64 -i cert.p12 \| pbcopy`) |
| `MACOS_CERTIFICATE_PASSWORD` | la contraseña con la que exportaste el `.p12` |
| `MACOS_SIGNING_IDENTITY` | `Developer ID Application: Tu Nombre (TEAMID)` |
| `NOTARY_APPLE_ID` | el Apple ID del equipo |
| `NOTARY_TEAM_ID` | el Team ID |
| `NOTARY_PASSWORD` | una contraseña específica de app (appleid.apple.com → Seguridad) |

Requiere una cuenta del Apple Developer Program (99 USD/año); sin ella el release no
puede firmarse ni notarizarse, y `ci.yml` sigue produciendo el `.app` ad-hoc como
artefacto de cada corrida. **El flujo de release está escrito pero no se ha ejecutado**:
depende de esos secretos, y la primera etiqueta será su primera prueba real.
