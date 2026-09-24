# EVA01

La capa de voz de tu Mac, escrita en Rust y en español.

- **Dictado** como Wispr Flow: mantienes una tecla, hablas, suelta, y el texto sale
  limpio y puntuado donde tengas el cursor. Todo en el equipo (Canary + Apple
  Intelligence); nada sale de tu Mac salvo que lo actives tú.
- **Órdenes** con una palabra de activación: «*Adán, abre Brave*», «*Adán, cierra
  Spotify*», «*Adán, busca el clima de mañana*».
- **Tareas para agentes**: «*Adán, arregla el test de login*» las despacha a Codex o
  Claude Code, en una rama de git desechable, mientras sigues dictando.
- **Edición**: selecciona texto y di «*Adán, hazlo más formal*».
- **Servidor MCP**: los agentes pueden abrir apps, pegar texto o preguntarte algo por
  las mismas herramientas (y el mismo control de permisos) que tus órdenes de voz.

Requiere una Mac con Apple Silicon, ~1,5–2 GB de RAM libre (los modelos de voz se quedan cargados;
en reposo el proceso no gasta CPU) y **macOS 26** (Apple Intelligence activado, con
español, para el formateo con contexto; sin él, EVA01 sigue dictando con reglas).

## Instalar

```bash
git clone https://github.com/raulisai97/eva01 && cd eva01
sh packaging/build-app.sh            # compila y arma target/EVA01.app (firma ad-hoc)
cp -R target/EVA01.app /Applications/
```

Necesitas Rust (`rustup`) y las Command Line Tools con el SDK de macOS 26
(`xcode-select --install`).

Instala el modelo de voz (una vez, ~940 MB) y revisa que todo esté listo:

```bash
E=/Applications/EVA01.app/Contents/MacOS/eva
$E model install canary-1b-flash
$E model install canary-180m-flash     # recomendado: entiende mucho mejor las órdenes cortas
$E doctor
```

`eva doctor` te dice qué falta y cómo arreglarlo. Cuando esté en verde:

```bash
open /Applications/EVA01.app
$E startup enable                    # opcional: abrirlo al iniciar sesión
```

### Permisos (la primera vez)

| Permiso | Para qué | Dónde |
|---|---|---|
| Micrófono | oírte | lo pide macOS solo |
| Accesibilidad | la tecla fn, pegar con Cmd+V y leer la selección | Ajustes → Privacidad y seguridad → Accesibilidad → activa EVA01 y vuelve a abrirlo |

Con la firma ad-hoc, macOS puede olvidar el permiso al recompilar; una firma con
Developer ID lo mantiene (`packaging/README.md`).

Si usas la tecla fn: Ajustes → Teclado → «Pulsar la tecla 🌐 para» → **No hacer nada**.

## Usarlo

| Haces | Pasa |
|---|---|
| Mantienes **fn** y hablas | el texto aparece donde tengas el cursor |
| «Adán, abre Brave» | abre la app |
| «Adán, arregla el bug del login» | una tarea para Codex / Claude Code en `eva/xxxxxxxx` |
| «Adán, usa Claude y…» | fuerza el agente |
| «Adán, continúa» | reanuda la última sesión |
| Seleccionas texto, «Adán, hazlo más corto» | reescribe la selección |
| **⌘↩** / **⌘⎋** | contestas sí / no cuando EVA01 pregunta (nunca por voz) |
| **⌃⌥⌘M** (o el menú) | marcas el último dictado como mal transcrito: guarda su audio para medirlo (`eval/README.md`) |

El ícono de la barra de menú muestra el estado (reposo, escuchando, trabajando, requiere
atención) y las tareas en curso, con «Cancelar tareas».

## Tus propias órdenes

En `config.toml`, cada `[[commands]]` es una orden tuya que hace **una** cosa:

```toml
[[commands]]
say = "mi correo"                      # «Adán, mi correo»
insert = "yo@ejemplo.com"              # pega este texto, tal cual

[[commands]]
say = "modo enfoque"
open = ["Notion", "Spotify", "https://calendar.google.com"]   # apps y direcciones, en orden

[[commands]]
say = "revisa los tests"
task = "corre los tests del proyecto y dime cuáles fallan y por qué"   # para el agente
```

`also = ["mi mail"]` añade otras formas de decir lo mismo. Además de `config.toml`, cada archivo
`*.toml` de `~/Library/Application Support/EVA01/commands/` (con solo `[[commands]]`) se carga al
iniciar: así se comparten, agregan y quitan órdenes de una en una, y un archivo roto se avisa por
su nombre sin afectar a los demás. `eva commands path` crea la carpeta con un ejemplo.

**Ver qué entiende:** `eva commands` lista las integradas (con ejemplo), las tuyas y de qué
archivo vienen; `eva commands apps` lista las aplicaciones detectadas en tu Mac y cómo decir
cada una. Las apps se detectan solas (no hay que configurarlas): si tienes Spotify, «Adán, abre
Spotify» funciona; si instalas una nueva se reconoce sin reiniciar. Si pides una que no está
instalada, EVA01 lo dice y pregunta si la busca en la App Store.

Acentos, mayúsculas y puntuación no importan, pero tiene que ser **toda** la orden («Adán, mi
correo es un desastre» no dispara «mi correo»). Pasan por el mismo gateway que cualquier orden,
así que `[gateway.voice]` las rige. Compruébalas sin ejecutarlas: `eva intent "Adán, mi correo"`.

## Calibrarlo a tu voz

`eva calibrate` te pide unas diez frases («Eva, abre Spotify», «Eva, cierra Spotify»…, con las apps
que de verdad tienes), oye cómo salen con tu micrófono y aprende cómo escribe el reconocedor **tu**
palabra de activación y los nombres de tus apps: si dices «Eva» y siempre sale «Ava», deja de ser
un fallo. No ejecuta nada de lo que dices. Lo aprendido se guarda en tu base de datos local y se
aplica al momento, incluso a la app que ya está abierta. Repítelo cuando cambies de micrófono o
de palabra de activación (`eva wake-word`).

## Configuración

Todo es opcional. Crea el archivo con ayuda y edítalo:

```bash
$E config edit
```

Vive en `~/Library/Application Support/EVA01/config.toml`. Ahí cambias la tecla, la
palabra de activación, tus propias órdenes, qué agente se prueba primero, qué pide confirmación (`auto` /
`confirm` / `block`, por acción y por origen: voz o agente), los estilos por app y el
modelo remoto opcional. Un error de tipeo no tumba nada: `eva doctor` lo señala y se usan
los valores por defecto.

## Comandos de `eva`

| | |
|---|---|
| `eva doctor [--smoke]` | diagnóstico; `--smoke` hace responder de verdad a cada agente |
| `eva model list / install / verify` | modelos de voz |
| `eva config path / show / edit` | configuración |
| `eva intent "Adán, abre Brave"` | cómo lo entendería EVA01, sin ejecutarlo; con `--run` lo ejecuta de verdad |
| `eva tasks` · `eva audit` | tareas de agente · lo que el gateway dejó pasar o rechazó |
| `eva history [--flagged]` | dictados recientes y los que marcaste como mal transcritos |
| `eva dictionary` · `eva wake-word` | diccionario personal · palabra de activación |
| `eva startup enable / disable / status` | abrir al iniciar sesión |

## Seguridad, en corto

- Lo destructivo no se ejecuta por voz: pide confirmación por **clic o atajo**, jamás por
  voz. Hay reglas que ninguna configuración afloja (apps protegidas, solo enlaces web,
  nada de pegar varias líneas en una terminal).
- Cada tarea corre en un worktree de git propio: lo que el agente malentienda no toca
  tu árbol.
- Si el foco está en un campo de contraseña, el dictado va al portapapeles, no se pega,
  y no queda en el historial.
- El historial de dictados (texto) se guarda solo en este equipo, 30 días, y se apaga con
  `[history] save_transcripts = false`. El audio no se guarda nunca, salvo el de lo que tú
  marcas como mal transcrito.
- Los agentes reciben las herramientas de EVA por un socket privado y con un token por
  ejecución; tu configuración global de Codex y Claude no se toca.
- El modelo remoto está **apagado** por defecto; si lo activas, esos textos salen de tu Mac.

## Si algo falla

| Síntoma | Causa probable |
|---|---|
| Mantengo fn y no pasa nada | falta Accesibilidad, o «Pulsar 🌐 para» no está en «No hacer nada» |
| Sale «ma ana» en vez de «mañana» | estás con `canary-180m-flash`; `eva model install canary-1b-flash` |
| «Adán, …» no despacha nada | `eva doctor --smoke`: el agente puede necesitar actualizarse o iniciar sesión |
| El formateo tarda la primera vez | Apple Intelligence carga su modelo (~2 s); EVA01 lo calienta al abrir |

Los registros están en `~/Library/Logs/EVA01/` (`eva-shell.log`, `eva-worker.log`). El
shell reinicia solo al worker si muere.

## Desarrollo

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
eval/generate-synthetic.sh && cargo run --release -p eva-eval -- --corpus eval/audio
cargo run -p eva-macos --example overlay_gallery -- /tmp/overlay   # el overlay en cada estado, a PNG
```

- `docs/PLAN.md` — el plan, el estado real y lo que se descubrió implementándolo.
- `docs/ENGINEERING.md` — las reglas que el repositorio hace cumplir.
- `eval/README.md` — el corpus, el WER y la latencia.
- `packaging/README.md` — firma, notarización y release.

Dos procesos por diseño: `eva-shell` (barra de menú, atajo, overlay) supervisa a
`eva-worker` (audio, voz, agentes), que es donde vive todo lo que puede fallar.

MIT.
