# EVA01 — Plan de ejecución en 10 fases

> Cuarta iteración. Reabro la decisión que las tres anteriores dieron por cerrada: **forkear Handy**.
> Verificado ahora: `transcribe-rs` (el motor STT de Handy) es una **librería Rust publicada de forma
> independiente** (MIT, v0.3.11, 146 k descargas) — no hace falta el resto de la app para usarla. Y las
> piezas de plataforma (`tray-icon`, `global-hotkey`, `objc2-app-kit` con NSPanel/NSPasteboard/NSWorkspace)
> también son crates sueltos, confirmados en docs.rs, sin necesitar Tauri.
>
> **Conclusión de esta iteración: no se forkea Handy.** Se escribe un binario nativo nuevo, pequeño,
> 100 % propio, sobre estas librerías. Se mantienen intactas las decisiones que nunca dependieron del
> fork: la arquitectura MCP, el trait `AgentProvider`, el diseño de gateway, y los hallazgos de español
> (ahora como lecciones de diseño, no como parches a código ajeno). Fecha: 21 sep 2026. Reemplaza al
> `PLAN.md` anterior.

---

## Estado de la implementación (v0.2 · 23 sep 2026)

Lo que sigue es lo que **existe y se probó de verdad**, no lo que el plan prometía. Las fases de abajo
(§4) se dejan como se escribieron: son el razonamiento; esta sección es el registro.

**Workspace:** 4 binarios (`eva-shell`, `eva-worker`, `eva-cli` → `eva`, `eva-eval`) y 10 crates
(`eva-audio`, `eva-text`, `eva-intent`, `eva-agents`, `eva-mcp`, `eva-macos`, `eva-store`, `eva-gateway`,
`eva-config`, `eva-ipc`). ~650 pruebas automáticas, `clippy -D warnings` limpio, más 9 pruebas marcadas
`#[ignore]` que solo corren con recursos reales (modelo STT, Apple Intelligence, CLIs de agente).

| Fase | Estado | Notas |
|---|---|---|
| 1 Spike | hecha | Canary vía `transcribe-rs`; ver hallazgo 5 (1B, no 180M) |
| 2 Esqueleto y supervisión | hecha | shell ↔ worker por JSON-lines, reinicio con backoff, watchdog por fase |
| 3 Dictado en español | hecha | Apple Intelligence + guarda de fidelidad; corpus de frases 40/40; `eva-eval` con línea base; cosecha: historial local, hotkey «esto salió mal» que guarda audio+texto, `eva history` con el conteo de retrabajos (`eval/README.md`) |
| 4 Intención | hecha | wake word con accent-folding; `AgentTask`/`EditSelection`; prefijo de proveedor ("usa Claude…") |
| 5 Acciones y gateway | hecha | política `auto/confirm/block` por acción y origen, pisos de seguridad no aflojables, auditoría |
| 6 Agentes | hecha | Codex y Claude Code, worktree por tarea, reanudar, cancelar por grupo de procesos, caída al siguiente proveedor |
| 7 Feedback y sesiones | hecha | overlay, tray con estados, tareas en segundo plano, avisos, `eva-store` v3 |
| 8 Servidor MCP | hecha | 10 herramientas, todas por el gateway, alcanzan al worker por un socket privado |
| 9 Estilo, contexto, edición | hecha | estilos por app, modo edición sobre la selección, remoto OpenAI-compatible **apagado por defecto** |
| 10 Manos libres | **diferida a propósito** | su disparador ("te sorprendes buscando la tecla") es del uso, no del calendario |

**Fuera del plan original, añadido porque el uso lo pidió:** CLI `eva` (`doctor`, `model list/install/verify`,
`startup enable`, `config`, `intent`, `tasks`, `audit`, `dictionary`, `wake-word`, `health`), gestión de modelos con verificación de tamaño y prueba de
voz real, configuración en `~/Library/Application Support/EVA01/config.toml` (claves desconocidas
avisan, nunca tumban), arranque al iniciar sesión (LaunchAgent), confirmaciones con hotkey.

### Lo que la implementación real cambió respecto del plan

1. **El worker no es de petición-respuesta.** Emite eventos en flujo y corre el trabajo largo como
   tareas rastreadas: puedes dictar mientras un agente trabaja, cancelar, o confirmar algo, sin que el
   bucle de comandos se bloquee. (Fase 2 lo describía síncrono.)
2. **El MCP no actúa por su cuenta.** El `eva-mcp` que lanza el agente es un proceso hijo del agente,
   así que no tiene overlay ni gateway propio: habla con el worker por un socket Unix privado (directorio
   0700, socket 0600, token de 256 bits por ejecución) y el worker decide. Sin el worker, `eva-mcp` en
   modo independiente niega toda acción sensible (`DenyAll`). Se inyecta por invocación
   (`--mcp-config`, `-c mcp_servers.eva…`), sin tocar la configuración global del usuario.
3. **Las banderas de los CLIs no eran las del plan** (verificadas contra los binarios reales):
   - `claude --resume X` no se puede combinar con `--session-id`; hay que usar solo `--resume`.
   - Codex reanuda con `codex exec resume <ID> <PROMPT>`; ese subcomando no acepta `-C` ni `-s`, así que
     se corre en el directorio de trabajo guardado y `-c sandbox_mode="workspace-write"`.
   - Codex asigna su propio identificador de sesión (`thread.started`); el de EVA no aplica.
   - Cancelar exige matar el **grupo** de procesos (`-pid`, luego `pid`; SIGTERM → SIGKILL a los 3 s):
     los CLIs lanzan hijos que sobreviven si solo se mata al padre.
4. **El prompt de Apple Intelligence tiene que ser una plantilla, no una instrucción.** Con
   "Transcripción: … / Corregida:" y ejemplos, la aceptación pasó de 23/40 a 40/40; cualquier pista de
   estilo dentro del prompt hacía que el modelo *contestara* el dictado. Los estilos (casual, formal,
   terminal) se aplican con reglas deterministas **después**.
5. **Un modelo de voz más pequeño no es "casi igual".** `canary-180m-flash` pierde la ñ ("ma ana");
   `canary-1b-flash` no. `eva doctor` avisa si el 180M está activo y `eva model install` instala el 1B.
6. **Canary se come la primera palabra** si el audio arranca sin silencio ("Hay que…" → "Que…"). Se
   añaden 300 ms de silencio antes y 200 después dentro de `CanarySpeechToText` (WER 6,6 % → 4,2 % en el
   corpus sintético). Audios de ~0,5 s siguen fallando: es límite del modelo.
7. **La primera llamada a Apple Intelligence cuesta ~2 s** (carga del modelo) y las demás ~0,7 s: el worker
   la calienta al arrancar. Con eso, voz+formato mide p50 ≈ 1,0 s y p95 ≈ 1,24 s (12 muestras
   sintéticas): la meta de 1,2 s de §7 queda **al borde, no cumplida con holgura**.
8. **La confirmación nunca es por voz**, y el gateway impone pisos que ninguna configuración afloja:
   apps protegidas (gestores de contraseñas…), solo esquemas web en `open_url`, y nada de pegar
   multilínea en una terminal. Si el foco está en un campo seguro (`IsSecureEventInputEnabled`), el texto
   va al portapapeles en vez de pegarse.
9. **Guarda de fidelidad del formateador** (no estaba en el plan): la salida solo puede usar palabras de
   la entrada (se restauran tildes de palabras interrogativas y se pueden borrar muletillas y palabras de
   número/símbolo); sin markdown, sin saltos de línea, sin MAYÚSCULAS. Si falla, se cae al texto por
   reglas. Es lo que evita que un modelo "servicial" reescriba lo que dijiste.

10. **El remuestreo del micrófono estaba mal, y solo se vio midiéndolo con una señal conocida.** La primera
    versión interpolaba linealmente *cada buffer de cpal por separado* y sin filtro: con los buffers de 512
    cuadros a 48 kHz que entrega el micrófono de esta misma Mac, cada callback redondeaba 170,67 muestras a
    171 (audio estirado y una muestra repetida cada ~10 ms: un tono de 440 Hz conservaba el 2 % de su
    energía) y un tono de 12 kHz, que 16 kHz no puede llevar, pasaba entero como uno de 4 kHz. `rubato` ya
    era dependencia y el plan lo nombraba; ahora `StreamResampler` (FFT síncrono con estado entre llamadas)
    da pureza > 0,999 con cualquier tamaño de buffer, atenúa lo que excede Nyquist > 34 dB y conserva los
    niveles de la banda de voz (±3 %). Lo cubre una prueba con un flujo estéreo de 48 kHz en buffers de 512.
    `eva doctor` ahora muestra el micrófono y su frecuencia (sin abrirlo: no pide permiso ni enciende el
    indicador).

### Lo que falta y por qué

- **Notarización y distribución firmada:** necesita un Apple Developer ID (decisión #7). El script de empaquetado
  está probado; el workflow de release está escrito pero **sin ejecutar** y espera los secretos
  (`packaging/README.md`).
- **Verificación de extremo a extremo de la GUI con un agente real:** en esta máquina ambos CLIs de agente
  están rotos (lo que `eva doctor` reporta y explica), así que el circuito MCP se probó hasta el socket y
  el arranque del agente, no hasta una llamada de herramienta hecha por el modelo.
- **Corpus real:** el sintético (`say`) es piso de humo. La cosecha con tu voz es lo que fija la línea base.
- **Fase 10 y GUI de ajustes:** diferidas (§8, decisión #9).

---

## 0. La pregunta que reabro: ¿fork o nativo?

Cada iteración anterior preguntó "¿qué le quitamos a Handy?" y nunca "¿hace falta Handy?". Vale la pena hacerse la segunda, porque el costo de un fork no es el código que tomas — es el que te comprometes a reconciliar **para siempre**.

### Lo que el fork realmente costaba

| Costo del fork | Tamaño real |
|---|---|
| Archivos Rust que pasas a mantener | 385 archivos, ~900 KB de código fuente |
| Commits/mes que hay que reconciliar | 42 |
| Toolchain adicional solo para la UI | Tauri 2 + React + TypeScript + Vite + **bun** + `node_modules` |
| Dependencia de código ajeno **sin mergear** | 3 PRs draft de otro autor (#1469, #1610, #618) que pueden cerrarse, reescribirse o cambiar de forma en cualquier momento |
| Gobernanza que hay que sostener | `UPSTREAM.md`, presupuesto de <200 líneas de divergencia, merge semanal, para siempre |
| Funcionalidad que traes gratis y no necesitas | catálogo de 69 modelos con UI de descarga, tray i18n, instalador NSIS de Windows, `a2a.rs`, grabación de reuniones |

Ese último punto es la señal más clara de sobre-ingeniería: **estabas a punto de mantener un fork de una aplicación completa para quedarte con tres módulos.**

### Lo que en realidad hace falta

| Necesidad | Antes (plan con fork) | Ahora (verificado) |
|---|---|---|
| Motor STT multi-modelo (Parakeet, Canary, Cohere, Whisper) | "hay que forkear Handy, que envuelve esto" | **`transcribe-rs`** es el crate que Handy mismo usa por debajo — se agrega con `cargo add`, sin la app alrededor |
| Icono de bandeja + menú | "lo da Tauri" | `tray-icon` (crate del equipo de Tauri, **standalone**, sin el framework) |
| Atajo global (`fn`, `⌥Space`) | "lo da `handy-keys`, dentro del fork" | `global-hotkey` (mismo equipo, standalone) |
| Overlay que no roba foco | "NSPanel vía `tauri-nspanel`" | `objc2-app-kit` expone `NSPanel` directo — confirmado en docs.rs |
| Pegado + restaurar portapapeles | "`paste_tx` de Handy, leer con cuidado" | `objc2-app-kit` expone `NSPasteboard` directo; el patrón de "esperar el recibo" se **diseña una vez, bien, propio** (~150-250 líneas) |
| App en primer plano / abrir apps | "`active_app.rs` de OpenFlow" | `objc2-app-kit` expone `NSWorkspace` directo |
| VAD (Silero) | "el de Handy" | `voice_activity_detector` (crate independiente, 157 k descargas, Silero) |
| Base de datos de historial | "la SQLite de Handy" | `rusqlite` (109 M descargas, la misma librería que usa Handy por debajo) |
| Notificaciones | "las de Tauri" | `notify-rust` (14 M descargas, standalone) |

**Todo lo que hacía valioso al fork resulta ser, pieza por pieza, una librería que se puede pedir con `cargo add`.** No hay ninguna parte de Handy que sea indivisible de su app Tauri, excepto la propia app Tauri.

### El trade-off, sin maquillar

Ir nativo no es gratis. Esto es lo que se pierde, dicho sin rodeos:

- **Cero fixes gratis** de la comunidad de Handy sobre casos raros de pegado, VAD o compatibilidad de apps. Ahora los descubres y los arreglas tú. (Mitigado en parte: el motor STT, la parte más difícil, sigue mejorando gratis vía `cargo update` de `transcribe-rs`, porque es un crate mantenido por separado.)
- **Windows deja de ser "cambiar una carpeta"**: con Tauri, cross-platform es casi gratis; yendo nativo sobre `objc2`/AppKit, Windows es una implementación paralela completa (`eva-platform-windows` contra Win32/WinRT) cuando llegue. El plan ya decía "Windows después"; ahora ese "después" es más caro, hay que decirlo con claridad.
- **No hay GUI de settings el día 1.** Se reemplaza por un archivo de configuración + una CLI propia (`eva config …`). Es una ventaja para el MVP (menos por construir) pero una ventana nativa de settings es trabajo real cuando se necesite.
- **Sin catálogo de 69 modelos con descarga automática.** Se reemplaza por 1-2 modelos fijados a mano y un comando `eva models pull <id>`.
- **Curva de aprendizaje de `objc2`** si es la primera vez que se hace bridging Rust↔Objective-C. Es una API bien tipada y madura (116 M descargas), pero es un paradigma distinto a Tauri.

A cambio: **cero fork que reconciliar, cero dependencia de PRs draft ajenos, un binario mucho más liviano y rápido de arrancar** (sin motor de WebView, sin puente a JavaScript, sin `node_modules`), compilaciones más rápidas, y **código 100 % propio, entendido de punta a punta** — exactamente lo que se pidió: Rust nuevo, rápido, simple.

---

## 1. Lo que se mantiene igual (nunca dependió del fork)

Estas piezas del plan anterior eran correctas independientemente de forkear o no, y siguen igual:

| # | Hallazgo | Sigue vigente porque… |
|---|---|---|
| 1 | El nivel 1 (router LLM) no hace falta: el agente, con las herramientas MCP de EVA, **es** el nivel 1 | Es una decisión de arquitectura de intención, no de plataforma |
| 2 | `rmcp` 3.4.0 es el SDK oficial de MCP en Rust, y ambos CLIs hablan MCP en las dos direcciones | Ídem — es la superficie abierta, no depende de qué shell la hospeda |
| 3 | Los CLIs de Codex y Claude Code ya traen sandbox, sesiones, background y salud (tabla §2) | Verificado con `--help` real en esta Mac, nada que ver con Handy |
| 4 | `claude -w/--worktree` da una rama desechable por tarea dictada | Propiedad del CLI, no del shell |
| 5 | Apple Intelligence soporta español desde abril 2025 y corre on-device | Propiedad del sistema operativo, se usa igual desde un binario nativo (de hecho, más directo: FFI propio a Foundation Models, sin pasar por la capa de "providers" de Handy) |
| 6 | Los muletillas del español ("este", "pues", "bueno") son palabras léxicas legítimas y un regex ciego las corrompe | Es una lección de diseño de texto, y **se escribe bien desde el día 1** en `eva-text`, en vez de heredar un motor de reglas ajeno y parcharlo |
| 7 | Un wake word con tilde (`"Adán"`) necesita comparación con *accent folding*, o falla en silencio | Se **escribe correcto desde el día 1** en el gate propio — ya no hay que parchear el `strip_wake_word` de un PR ajeno |
| 8 | El diccionario personal necesita Unicode completo, no ASCII, y Levenshtein/Jaro-Winkler generalizan mejor a español que Soundex (fonética inglesa) | Se **diseña bien desde el día 1**: no hay bug que arreglar porque no se hereda el bug |

La diferencia de fondo: antes, cada hallazgo de "esto está mal en Handy para español" se resolvía como *parche + PR a upstream*. Ahora se resuelve escribiendo la pieza correcta una sola vez, sin arrastrar el resto del archivo donde vivía el bug.

---

## 2. Lo que los CLIs de agente ya hacen (interrogados aquí, hoy)

Esta tabla es el cimiento de la fase 6, y no depende de nada de lo anterior. Cada celda salió de `--help` en esta máquina: `codex-cli 0.142.0`, `Claude Code 2.1.267`.

| Necesidad | Codex 0.142.0 | Claude Code 2.1.267 |
|---|---|---|
| Ejecución headless | `codex exec --json` (JSONL) | `claude -p --output-format stream-json --verbose` **(el `--verbose` es obligatorio: verificado, falla sin él)** |
| Directorio de trabajo | `-C, --cd <DIR>` · `--add-dir` | `--add-dir` · `-w/--worktree` |
| Sandbox / permisos | `-s {read-only, workspace-write, danger-full-access}` | `--permission-mode {acceptEdits,auto,bypassPermissions,manual,dontAsk,plan}` · `--allowedTools` / `--disallowedTools` |
| Sesiones | `codex exec resume <id> \| --last` · `fork` · `archive` | `--resume` · `--continue` · `--session-id <uuid>` (¡lo asignas tú!) · `--fork-session` |
| Segundo plano | `app-server`, `exec-server` (experimentales) | `--bg` + `agents --json` + `attach` / `logs` / `stop` / `rm` / `respawn` |
| Salida estructurada | `--output-schema <FILE>` | `--output-format json` |
| Entrada en streaming | — | `--input-format stream-json` (sesión viva, sin resume) |
| MCP como servidor | `codex mcp-server` (stdio) | `claude mcp serve` |
| MCP como cliente | `codex mcp add/list/get/remove` | `--mcp-config <json>` · `--strict-mcp-config` · `claude mcp add` |
| Salud / sesión | `codex doctor` · `codex login status` | `claude doctor` · `claude auth` |
| Extra útil | `codex sandbox <cmd>` (ejecuta cualquier comando en su sandbox) · `codex apply` | `--brief` (habilita que el agente te hable) · `--bare` (modo automatización) |

**La consecuencia de diseño:** EVA no orquesta agentes, **los invoca bien**. Su trabajo es elegir proyecto, elegir banderas, normalizar eventos y enseñar el resultado.

---

## 3. La arquitectura nativa

```
eva01/                          (repo nuevo, tuyo, MIT desde la línea 1 — sin fork)
├── Cargo.toml                   workspace
├── bins/
│   ├── eva-shell/                el proceso que el usuario ve: tray-icon + global-hotkey + overlay
│   │                             (NSPanel). Supervisa a eva-worker (ver §3.3). ~300 líneas, deps mínimas.
│   └── eva-worker/                el proceso que hace el trabajo real; hijo de eva-shell, hablan por
│                                  stdio en JSON-lines. Si muere, eva-shell lo reinicia.
├── crates/
│   ├── eva-audio/                 cpal (captura) + rubato (resample) + voice_activity_detector (Silero)
│   │                              + transcribe-rs (Parakeet/Canary/Cohere/Whisper)
│   ├── eva-text/                  muletillas (universal + es, consciente de posición) + diccionario
│   │                              (Jaro-Winkler/Levenshtein vía strsim + NFD, Unicode desde el día 1)
│   │                              + formateador (bridge propio a Apple Intelligence, con timeout)
│   ├── eva-intent/                 wake-word gate (accent-fold desde el día 1) + reglas + índice de apps
│   ├── eva-agents/                 trait AgentProvider · codex · claude_code · detección · sesiones
│   ├── eva-mcp/                    servidor MCP (rmcp): open_app, insert_text, notify, ask_user_confirmation…
│   ├── eva-macos/                   objc2-app-kit: overlay (NSPanel), paste confiable (NSPasteboard + receipt),
│   │                                frontmost (NSWorkspace), secure input, tray (tray-icon), hotkey (global-hotkey)
│   └── eva-store/                   rusqlite: historial, auditoría, settings, versión de esquema
├── swift/                         bridge de Apple Intelligence (FFI) — el único Swift del proyecto, ~100 líneas
├── eval/                          corpus propio + runner de métricas (usa eva-audio directo, sin CLI externa)
└── commands/                      comandos personalizados en YAML
```

Sin Tauri, sin React, sin `node_modules`. Un crate se parte cuando pasa de ~2.000 líneas o cambia su set de dependencias — el mismo criterio de antes, aplicado a un árbol que ahora es 100 % propio. La división en dos binarios (`eva-shell`/`eva-worker`) es la única pieza nueva frente a la iteración anterior, y está motivada en §3.3: es lo que hace que un crash del motor STT no se lleve por delante el atajo global.

### 3.1 MCP sigue siendo la arquitectura abierta

Esto no cambia con el pivote — MCP nunca dependió de Tauri ni de Handy:

```
                     ┌─────────────── EVA01 (binario nativo) ───────────────┐
  voz ──► STT ──►    │  dispatch → intent → gateway                        │
                     │      │                                              │
                     │      ├── eva-macos (overlay, paste, frontmost)      │
                     │      │                                              │
                     │      ├── eva-agents ──MCP client──► cualquier agente
                     │      │                 (Codex, Claude, el que venga)
                     │      └── eva-mcp ────MCP server───► los agentes usan
                     └───────────────────────────────────────┘   tu escritorio
```

- **EVA como servidor MCP** (`rmcp`): `open_app`, `open_url`, `insert_text`, `get_active_window`, `get_selection`, `notify`, `speak`, `ask_user_confirmation`, `list_projects`.
- **EVA como cliente**: `AgentProvider` se implementa sobre CLI o sobre MCP. Añadir el agente que salga en 2027 es un archivo nuevo en `eva-agents`.
- **EVA como CLI**: toda orden de voz debe poder probarse como comando (`eva intent "abre brave"`). Esto es lo que hace testeable la capa de intención sin grabar audio, y ahora es más simple aún: `eva` es directamente el binario del workspace, no un flag especial de una app Tauri.

### 3.2 No se escribe el router LLM (nivel 1) — sigue igual

```
"Adán, …"
   ├─ nivel 0: reglas + índice de apps + fuzzy   → milisegundos, sin red, determinista
   └─ sin match → el agente, con el transcript como prompt
                  y con las herramientas MCP de EVA en la mano
```

El agente, con la fase 8, **es** el nivel 1: ya sabe descomponer "abre Brave y busca X", ya está instalado, ya lo mantiene otro. Escribir un router propio sería mantener una peor copia de algo que ya corre. Queda como bandera si el eval mostrara un hueco real — `rmcp`/`ort` ya están en el árbol para eso, no hace falta escribir nada por adelantado.

### 3.3 Ingeniería de confiabilidad: por qué esto no se cae

Al no forkear, EVA01 pasa a ser 100 % responsable de su propia estabilidad — nadie más audita el bridging a AppKit ni absorbe un crash de la nada. "Simple" y "frágil" no son la misma palabra, así que esto se diseña, no se espera. Cinco decisiones concretas, ninguna es "sobre-ingeniería": son el patrón estándar de las apps de escritorio que de verdad no se caen (es como Chrome separa pestaña de navegador, o VS Code separa su "extension host" del editor).

**1. Dos procesos, no uno — el fallo se contiene, no se propaga.**

```
┌─────────────── eva-shell (proceso 1) ───────────────┐        ┌──────── eva-worker (proceso 2) ────────┐
│  tray-icon + global-hotkey + overlay (NSPanel)      │◄──IPC──►│  audio · STT (transcribe-rs/ONNX) ·    │
│  ~300 líneas, dependencias mínimas, CASI NUNCA       │  stdio  │  eva-text · eva-intent · eva-agents ·  │
│  toca código que puede tronar (sin ONNX, sin FFI)    │  JSONL  │  eva-mcp · eva-store                   │
└──────────────────────────────────────────────────────┘        └─────────────────────────────────────────┘
        │ supervisa: reinicia el worker si muere,                        ↑ aquí vive TODO el código
        │ con backoff, y el tray/hotkey siguen vivos                       que puede fallar de verdad
```

`eva-shell` es el único proceso que el usuario "ve" (ícono de bandeja, atajo, overlay) y su única responsabilidad además de eso es **supervisar**: lanza `eva-worker` como hijo, le habla por stdio en JSON-lines (el mismo patrón que ya se usa para `eva-mcp`, no es una idea nueva), y si el worker muere —por lo que sea: un crash del runtime ONNX con un audio corrupto, un `panic!` en una dependencia, un hang en la FFI de Swift— **lo reinicia con backoff exponencial** y se pierde como mucho la orden que estaba en curso. El tray icon nunca desaparece, el atajo nunca deja de responder. Es la diferencia entre "se perdió una transcripción" y "hay que forzar Salir desde el Monitor de Actividad".

**2. Nada de `unwrap`/`panic!` fuera de tests — el compilador lo exige, no la disciplina.**

```toml
# en cada crate de eva-worker (lib.rs)
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
```

Todo error es un `Result<T, EvaError>` (`thiserror`, un enum tipado por crate + conversiones `From`). Además, cada unidad de trabajo (una transcripción, una llamada a herramienta MCP, un turno de agente) corre envuelta en `catch_unwind` como segunda red: si algo entra en pánico de todos modos —una dependencia de terceros, un caso límite no previsto—, se captura, se registra y falla *esa tarea*, no el proceso completo. Cinturón y tirantes: el proceso ya está aislado por el punto 1, y además cada tarea dentro de él está aislada por esto.

**3. La frontera FFI (Swift, ONNX) se trata como frontera de confianza, no como una llamada de función más.**

El bridge de Apple Intelligence y `transcribe-rs`/`ort` son los dos puntos donde Rust cruza a código que no es Rust — ahí es donde vive el riesgo real de un crash duro (no un panic capturable, un *abort*). Dos reglas: **toda llamada FFI corre con `tokio::time::timeout`** en un hilo dedicado (si Foundation Models se cuelga, el worker no se cuelga con él — a los N segundos se cancela y se degrada, ver punto 5); y **el formateador nunca es la única vía**: si Apple Intelligence no responde o no está disponible, el texto sale con la limpieza de reglas (interjecciones puras, §3 de la fase 3) en vez de no salir nada.

**4. Estados explícitos con vigía (*watchdog*), no banderas sueltas.**

El overlay de la fase 7 (`IDLE → ESCUCHANDO → PENSANDO → EJECUTANDO → ✓/✗`) es una máquina de estados tipada (`enum` + `match` exhaustivo, el compilador rechaza un caso no manejado), no variables booleanas repartidas por el código — esa es la diferencia entre un bug que el compilador atrapa en `cargo build` y uno que aparece a las dos semanas de uso. Cada estado que no sea `IDLE` lleva un temporizador: si algo se queda en `PENSANDO` más de N segundos sin transición (un agente que no responde, una llamada STT que nunca vuelve), se fuerza la vuelta a `IDLE`, se registra, y se avisa — en vez del bug más molesto de estas apps: quedar "escuchando" para siempre y no enterarte hasta la próxima vez que lo necesitas.

**5. Degradación explícita, nunca silenciosa.** Para cada pieza, qué pasa cuando falla está decidido de antemano, no improvisado:

| Si falla… | EVA01 hace… | Nunca hace… |
|---|---|---|
| Apple Intelligence no responde/no está | Texto con limpieza de reglas únicamente | Dictar en silencio sin avisar que el formateo se saltó |
| El modelo STT no carga | Overlay en rojo + notificación clara al primer intento de grabar | Fingir que grabó y devolver texto vacío |
| El CLI de un agente no está o cambió de banderas | Mensaje explícito + abre config | Reintentar en bucle o fallar sin explicación |
| Una herramienta MCP falla | El agente recibe un error MCP normal | Colgar la llamada sin responder |
| `eva-worker` muere | `eva-shell` lo reinicia, overlay muestra "Reiniciando…" | Dejar el tray icon como si nada, sin dar señal |
| La SQLite de `eva-store` está corrupta | `PRAGMA integrity_check` al arrancar; si falla, se aparta el archivo viejo y se crea uno nuevo | Crashear el arranque por un archivo dañado |

### 3.4 Observabilidad y pruebas — "fácil de mantener" es una propiedad que se construye

**Logging estructurado desde el primer commit**, no como añadido al final: `tracing` en cada crate, con salida a `~/Library/Logs/EVA01/eva.log` (rotación diaria). Cuando algo falle dentro de tres meses, la pregunta "qué pasó" tiene una respuesta en un archivo, no en la memoria.

**`eva doctor`** (el mismo patrón que `codex doctor`/`claude doctor`, aplicado a EVA01 mismo): un comando que en un solo vistazo dice si el sistema está sano — CLIs detectados y su versión vs. la "conocida-buena" fijada en config, modelo STT cargado, permisos de macOS (micrófono, accesibilidad), espacio en disco para modelos, y las últimas N líneas de error del log. Es el primer paso de cualquier "no me funciona", tuyo o de quien lo use después.

**Testing por crate, con la herramienta que corresponde a cada uno** — no "cobertura" como meta abstracta, sino la prueba que de verdad atrapa el bug de esa pieza:

| Crate | Qué se prueba y cómo |
|---|---|
| `eva-text` | **Property-based** (`proptest`): para cualquier cadena, el *accent-folding* es idempotente y no rompe ASCII; el fuzzy match nunca entra en pánico con Unicode arbitrario (emoji, CJK, control chars) |
| `eva-intent` | Fixtures texto→JSON (ya definido en fase 4): un archivo de casos que crece agregando una línea, corre en cada PR |
| `eva-macos` | Frontera angosta por `trait` (`trait Pasteboard { fn paste(&self, text: &str) -> Result<()>; }`) con un mock para probar la lógica sin AppKit real; una checklist manual de humo (las 5 apps de la fase 1) antes de cada release |
| `eva-agents` | `AgentProvider` mock para probar dispatch/sesiones sin gastar cuota real; un puñado de smoke tests contra los CLIs reales, aparte, manuales o en un job de CI que no bloquea (necesitan sesión iniciada) |
| `eva-worker` (integración) | Matar el proceso con `kill -9` en medio de una grabación y verificar que `eva-shell` lo reinicia y el overlay lo refleja — esto es una prueba de CI, no solo una idea |

**CI obligatorio en cada PR**: `cargo test --workspace`, `cargo clippy --workspace -- -D warnings` (con los `deny` del punto 2 de arriba, esto ya bloquea `unwrap`/`panic!` nuevos), y `cargo audit` (vulnerabilidades conocidas en dependencias — al ser dueño de todo el árbol de dependencias, alguien tiene que vigilarlas; que sea CI, no memoria).

**Los CLIs externos se tratan como dependencias versionadas, no como una constante.** Config guarda la versión "conocida-buena" de `codex`/`claude` con la que se probó cada plantilla de invocación. En cada arranque (y en "Re-detectar"), si la versión instalada difiere, se corre el smoke test automáticamente y se avisa **antes** de que falle a mitad de una tarea real — convierte el riesgo más alto del proyecto ("los CLIs cambian de bandera", ya marcado como probabilidad alta) de silencioso a detectado.

---

## 4. Las 10 fases

Las fases 1-6 son el MVP (≈4 semanas). Las 7-9 lo vuelven un producto. La 10 se abre sola cuando el uso lo pida.

---

### Fase 1 — Spike con los audios reales · 2 días

**Objetivo:** cerrar con evidencia las decisiones que cuestan caro si se equivocan, antes de escribir la app.

| Prueba | Cómo (nativo, sin CLI de terceros) | Criterio | Si falla |
|---|---|---|---|
| **STT en español real** | `cargo new eva-spike`, `cargo add transcribe-rs --features onnx,whisper-cpp`, cargar 3-4 modelos (§5) y correr tus 10 audios (jerga, repos, inglés mezclado) | Usable sin editar en ≥8/10 | El diccionario (`eva-text`) sube a bloqueante de la fase 3 |
| **Pegado en TUS apps** | Prototipo de 30 líneas con `objc2-app-kit`: `NSPasteboard` set + `CGEvent` Cmd+V + restaurar, contra VS Code, Ghostty/Terminal, Slack, Brave, Notion | Pega íntegro y el portapapeles queda intacto en las 5 | El patrón de "recibo" (esperar a que la app lea antes de restaurar) sube a obligatorio en fase 2, no opcional |
| **Firma y permisos** | Build firmado, reinstalar 3×, revisar Accesibilidad e Input Monitoring | Sobreviven sin reconceder | Apple Developer ID pasa a bloqueante el día 1 |
| **`objc2`/AppKit, primera vez** | Un `main.rs` de 40 líneas: `tray-icon` con un ítem, `global-hotkey` registrando `fn`, y que al soltarla imprima "hola" | Compila y responde al atajo | Si la curva de aprendizaje es más alta de lo esperado, se reserva medio día extra en la fase 2 — mejor saberlo aquí que a mitad de fase |

**Entregable:** modelo por defecto elegido con números tuyos, y un `main.rs` mínimo que ya prueba las tres piezas nativas más riesgosas (audio, paste, tray+hotkey) antes de construir nada encima.
**Hecho cuando:** ningún ítem del plan sigue apoyado en un supuesto, y viste con tus ojos un tray icon y un atajo global funcionando sin Tauri.

---

### Fase 2 — El esqueleto nativo, ya con supervisión · semana 1

**Reutiliza:** `tray-icon`, `global-hotkey`, `objc2-app-kit`, `tracing`, `thiserror`, la plantilla de release de Wisper (GitHub Actions), `self_update` (en vez de `tauri-plugin-updater`).
**Escribe:** el workspace completo, `eva-shell`, `eva-worker`, el protocolo IPC entre ambos, `eva-macos` con el overlay y el paste.

La separación en dos procesos (§3.3) se instala **aquí**, no después: retrofitearla sobre un binario monolítico que ya hace de todo es mucho más caro que empezar así.

- `Cargo.toml` workspace con los 2 binarios + 7 crates de §3. Sin Tauri, sin `package.json` en la raíz. `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` en cada crate de `eva-worker` desde el primer commit.
- `eva-shell`: `LSUIElement` (sin ícono en el Dock, como Handy), `tray-icon` con menú mínimo (Preferencias, Salir), `global-hotkey` para `fn` sostenida, y el supervisor: lanza `eva-worker` como hijo, lee su stdout línea a línea, y si el proceso termina lo reinicia con backoff exponencial (1 s, 2 s, 4 s… con un techo).
- `eva-worker`: arranca vacío por ahora (las fases siguientes lo llenan), pero ya habla el protocolo IPC (JSON-lines por stdio) y ya tiene `tracing` escribiendo a `~/Library/Logs/EVA01/`.
- `eva-macos::overlay`: un `NSPanel` sin borde, siempre-encima, que no roba foco (`NSPanel` con `nonactivatingPanel` — la misma técnica que `tauri-nspanel` usa por debajo, ahora directa). Vive en `eva-shell`, porque el overlay tiene que poder mostrar "Reiniciando…" aunque el worker esté muerto.
- `eva-macos::paste`: el patrón de "recibo" diseñado propio — `NSPasteboard` guarda el `changeCount` antes de pegar, sintetiza Cmd+V con `CGEvent`, y solo restaura el portapapeles original cuando el `changeCount` de destino confirma la lectura (o vence un timeout corto). Es la misma idea que "reliable paste" de Handy, pensada y escrita de cero.
- **Firma estable + notarización en el CI desde ya.** Sin firma pierdes los permisos de macOS en cada build; retrofitear notarización después cuesta más que hacerla hoy.
- CI: `cargo test --workspace` + `cargo clippy --workspace -- -D warnings` + `cargo audit`, obligatorios desde el primer PR.

**Hecho cuando:** un build firmado muestra el ícono de bandeja, `fn` abre el overlay, pegar en las 5 apps de la fase 1 sigue funcionando desde el binario real, **y `kill -9` al proceso `eva-worker` mientras la app corre hace que el tray siga vivo y el worker reaparezca solo en unos segundos** — esta última prueba es la que confirma que el diseño de §3.3 funciona, no solo que está escrito.
**Riesgo:** el bridging `objc2` tiene detalles finos (retención de memoria, hilos principales de AppKit). Mitigación: el spike de la fase 1 ya lo tocó; si algo no cuadra, es el primer problema de la fase 2, no el último.

---

### Fase 3 — Dictado en español, correcto desde el día 1 · semana 1-2

**Reutiliza:** `transcribe-rs` (el modelo elegido en fase 1), `strsim` (Levenshtein + Jaro-Winkler), `unicode-normalization` (NFD), el bridge de Apple Intelligence.
**Escribe:** `eva-text` completo, el corpus, el runner de métricas, el prompt del formateador.

Esta es la fase que decide si "100 % en español, sin muletillas, como Wispr Flow" es cierto. Al escribir todo de cero, los bugs que auditamos en Handy (ver el histórico de este documento: el motor de reglas allí solo cubre en/de/fr; el diccionario personal excluye tildes porque exige ASCII puro; Soundex es fonética inglesa) **simplemente no existen aquí**, porque se diseña bien la primera vez:

1. **Modelo por defecto**: el que ganó la fase 1 (candidato: `canary-1b-flash`, §5), cargado vía `transcribe-rs` directo — sin capa de "catálogo" intermedia.
2. **Muletillas, con conciencia de posición.** Dos niveles:
   - *Interjecciones puras* (universales: "ehh", "ehm", "mmm") → regla simple, cero riesgo, se borran donde aparezcan.
   - *Muletillas léxicamente ambiguas del español* ("este", "pues", "bueno", "o sea", "digo") → **nunca** un regex global. Se le pasan al formateador con contexto (punto 4), que sabe distinguir "este coche" de "este, no sé" porque ve la frase entera.
3. **Diccionario personal (`custom_words`)**, diseñado en Unicode desde el día 1: clave de comparación con NFD (sin perder tildes/ñ) + Jaro-Winkler/Levenshtein de `strsim`. Sin Soundex — es fonética inglesa y no aporta en español; un solo algoritmo de distancia de edición, aplicado de forma consistente, es **más simple y generaliza mejor**. "García" corrige igual si se dictó "Garcia".
4. **Formateador con Apple Intelligence, encendido por defecto.** Bridge FFI propio a Foundation Models (~100 líneas de Swift, el único Swift del proyecto), on-device, cero red. Prompt propio: ortografía y puntuación (incluye `¿`/`¡`, que exige saber que la frase es pregunta antes de que termine — solo un formateador con contexto lo hace bien), números a dígitos con ejemplos en español, y las muletillas ambiguas del punto 2, con la instrucción explícita de "quítalas solo si no cambian el significado".
5. **`eval/`**: un binario propio en el workspace (usa `eva-audio` y `eva-text` directo, sin invocar un CLI externo) que corre el corpus y reporta WER, **p95**, y muletillas que sobrevivieron al texto ya formateado. Corre en CI.
6. **Cosecha**: cada transcripción guarda audio + crudo + formateado en `eva-store` (SQLite), y una hotkey marca "esto salió mal". El corpus crece solo mientras trabajas.

**Hecho cuando:** llevas 3 días sin volver al teclado, dictas "o sea, este, mándale el archivo a Juan" y sale limpio y puntuado, y tienes números base propios para comparar cada cambio futuro.

**§3B — Lo que se encontró implementando de verdad el bridge de Apple Intelligence (no en el spike, en el código real):**

El punto 4 está construido y funcionando (`crates/eva-text/src/apple_intelligence.rs`,
`swift/eva_formatter.swift`), pero probándolo a mano con frases reales
apareció un problema de fondo que el spike no había mostrado:
`LanguageModelSession.respond(to:)` enmarca cualquier texto como un turno de
chat, no como "datos a transformar" — así que con un prompt corto, correcto
en apariencia, el modelo a veces **respondía o cumplía** el dictado en vez
de solo corregirlo. Encontrado a mano, con la app real corriendo:

| Dictado | Con el prompt corto (roto) | Con el prompt + ejemplos + guarda (real) |
|---|---|---|
| "necesito tres archivos y dos carpetas para mañana" | inventaba una lista completa de nombres de archivo falsos | "Necesito tres archivos y dos carpetas para mañana." |
| "manda un correo a soporte diciendo que el servidor está caído" | escribía un correo completo, inventado | "Manda un correo a soporte diciendo que el servidor está caído." |
| "dile a maría que la reunión se movió a las tres" | "dile" → "mándale" (cambia la palabra, no solo la forma) | "Dile a María que la reunión se movió a las tres." |
| "cuando vas a llegar a la oficina" | sin ¿ de apertura | "¿Cuándo vas a llegar a la oficina?" |

La mitigación tiene dos capas, ninguna suficiente por sí sola:
1. **El prompt** (`swift/eva_formatter.swift`) pasó de un párrafo a reglas
   explícitas + 8 ejemplos entrada→salida, incluyendo casos que suenan a
   orden — esto arregló la mayoría de los casos pero no todos (un modelo
   pequeño on-device no sigue sus propias instrucciones con consistencia
   perfecta).
2. **Una guarda en Rust** (`is_plausible_correction` en
   `apple_intelligence.rs`): ninguna corrección legítima de este formateador
   (mayúsculas, puntuación, dígitos, quitar una muletilla) puede agregar
   palabras — solo mantenerlas o quitarlas. Una respuesta con más palabras
   que la entrada se rechaza y cae al `RuleOnlyFormatter`, sin importar qué
   tan fluida o convincente suene. Esto es lo que de verdad protege contra
   la próxima frase rara que el prompt no cubra, no el prompt en sí.

Ambas capas están probadas: `apple_intelligence.rs` tiene tests puros para
la guarda (sin necesitar el modelo real) y tests `#[ignore]` que sí llaman
al modelo real y verifican los dos casos de la tabla de arriba. Además, se
compone `RuleOnlyFormatter` sobre CUALQUIER salida exitosa de Apple
Intelligence (no solo como fallback) porque el modelo tampoco es consistente
capitalizando la letra real después de un ¿/¡ que él mismo acaba de agregar
— una normalización mecánica y sin riesgo, ya que nunca toca palabras ni
conteo.

**Empaquetado real, no solo compilado:** `crates/eva-text/build.rs` compila
`swift/eva_formatter.swift` a un dylib con un `-install_name` absoluto (útil
para `cargo build`/`test`/`run`, inútil una vez movido el binario), así que
`packaging/build-app.sh` lo copia a `Contents/Frameworks/` y reescribe esa
referencia a `@executable_path/../Frameworks/...` con `install_name_tool`.
Encontrado al probar el `.app` empaquetado de verdad, no asumido: con
`--options runtime` (Hardened Runtime) y firma ad-hoc, dyld rechazaba cargar
el dylib ("different Team IDs") porque una firma ad-hoc no lleva un Team ID
real compartido entre archivos — el script ahora solo activa Hardened
Runtime cuando se firma con una identidad real (`SIGNING_IDENTITY`), no con
la ad-hoc por defecto.

---

### Fase 4 — La capa de intención, sin efectos · semana 2

**Reutiliza:** las lecciones de diseño del PR #1469 de Handy (la idea del gate por prefijo es buena; el código no se toma porque es más simple escribirlo bien una vez que parchear un draft ajeno).
**Escribe:** `eva-intent` completo (~150-250 líneas) y la CLI de EVA.

- Regla de oro de esta fase: **no ejecuta nada**. Convierte texto en un `Intent` tipado y ya.
- El gate de wake word (~40-60 líneas) se escribe con *accent folding* desde la primera versión — NFD + comparación sin diacríticos — porque ya se sabe, por haberlo visto fallar en otro proyecto, que un wake word con tilde sin ese cuidado falla en silencio. Cero PRs draft de los que depender; cero riesgo de que alguien cierre o rediseñe el código del que dependes.
- `eva intent "abre brave"` imprime el intent en JSON. El eval de comandos son **fixtures de texto**, sin audio: rápido, determinista, corre en cada PR.
- `wake_word = "Adán"`, exigido para pegar. Captura siempre activa **apagada** por defecto (queda para la fase 10, con puerta de entrada propia).
- Tests explícitos: "Adán" con y sin tilde, "adan"/"ADÁN"/"Adán," todos disparan igual.

**Hecho cuando:** "Adán, …" nunca aparece pegado en un documento —tampoco cuando el STT lo transcribe sin tilde—, y el acierto de intents nivel 0 está medido sobre fixtures.

---

### Fase 5 — Acciones del SO y gateway · semana 3

**Reutiliza:** `objc2-app-kit::NSWorkspace` (frontmost, abrir apps, URLs). `eva-store` (SQLite) para la auditoría.
**Escribe:** `eva-macos::workspace`, el índice de apps, `eva-core::gateway`.

- Índice de `/Applications` con alias ("code" → Visual Studio Code, "brave" → Brave Browser). Abrir/cerrar vía `NSWorkspace` directo; URLs; búsqueda.
- `eva-core::gateway`: `permissions.yaml` con auto / confirmar / bloquear, auditoría completa (transcript → intent → decisión → resultado) en `eva-store`, y **confirmación solo por clic o hotkey, jamás por voz**.
- **Lista negra de verbos destructivos evaluada antes de las reglas**, con los homófonos del español (`borra`/`borrá`, `para`/`pará`, `mata`, `tira`): se bloquea aunque una regla hiciera match.
- App en primer plano (`NSWorkspace.frontmostApplication` + título de ventana) → proyecto inferido, para la fase 6.

**Hecho cuando:** "Adán, abre Brave / cierra Spotify / busca X" funciona en milisegundos y sin red, y lo destructivo no se ejecuta ni queriendo.

---

### Fase 6 — Agentes: un adaptador delgado · semana 3-4 · **cierra el MVP**

**Reutiliza:** las banderas de §2 (que es casi todo el trabajo difícil, y ya viene de los CLIs, no de ningún fork).
**Escribe:** `eva-agents` completo, ~350-450 líneas.

- Trait `AgentProvider` (`execute` · `stream_events` · `cancel` · `resume` · `status`) normalizando a `Started · Message · ToolCall · FileChanged · ApprovalRequired · Completed · Failed`.
- Plantillas (editables por config, porque los CLIs se mueven rápido):
  - `codex exec --json -C <proj> -s workspace-write`
  - `claude -p --output-format stream-json --verbose --add-dir <proj> --permission-mode acceptEdits --session-id <uuid>`
- **Detección oficial**: `codex doctor` / `claude doctor` / `claude auth`, con sondeo de PATH como respaldo. Prioridad **Codex ▸ Claude Code**, forzable por voz ("Adán, usa Claude y…").
- **Worktree por defecto para tareas dictadas** (`claude -w`; en Codex, un worktree que crea EVA con `git worktree add`). Una orden mal entendida toca una rama desechable, no tu árbol.
- **Proyecto activo por contexto** (de la fase 5): si la ventana enfocada es VS Code o una terminal dentro de un repo, ese es el proyecto. El escaneo de carpeta raíz se pospone.
- El fallback de §3.2: sin match de reglas + hay proyecto activo → al agente.

**Hecho cuando (aceptación del MVP):** una semana como única herramienta de dictado, **≥5 tareas despachadas por voz que no tuviste que rehacer**, y cero acciones destructivas sin confirmar.

---

### Fase 7 — Feedback, panel y sesiones · semana 4

**Reutiliza:** `claude --bg` + `agents --json --cwd` + `logs`/`stop`/`respawn`. `codex exec resume`. `say` (Mónica/Paulina, ya instaladas — verificado con `say -v '?'`). `notify-rust`.
**Escribe:** los estados del overlay, el registro de sesiones, el panel.

- Overlay (el `NSPanel` de la fase 2): la máquina de estados tipada de §3.3 punto 4 — `IDLE → ESCUCHANDO → PENSANDO → EJECUTANDO → ✓/✗`, con el *watchdog* que fuerza la vuelta a `IDLE` si algo se cuelga. Sin el feedback visual el despacho por voz se siente roto; sin el watchdog, un agente colgado te deja "escuchando" para siempre sin que te enteres.
- Panel de tareas alimentado por `claude agents --json` + un registro propio en `eva-store` para Codex. Las tareas corren en segundo plano: sigues dictando mientras el agente trabaja.
- Aviso al terminar: `notify-rust` + TTS corto con `say -v Mónica` — voz de español "premium" ya instalada, evitando las de familia "novelty" (Grandma, Rocko…) que también aparecen en `es_ES`/`es_MX` pero suenan a personaje.
- Sesiones: "Adán, continúa" → `codex exec resume <id>` / `claude --resume <uuid>`, con **UUIDs que asigna EVA** (`--session-id`), así la contabilidad es tuya.

**Hecho cuando:** lanzas tres tareas, sigues dictando, y las tres te avisan y se pueden retomar por voz. Y, a propósito: matas un agente a mitad de tarea (`kill -9` al proceso del CLI) y el overlay vuelve solo a `IDLE` en vez de quedarse en `EJECUTANDO` para siempre.

---

### Fase 8 — EVA como servidor MCP: la arquitectura abierta · semana 5

**Reutiliza:** `rmcp` 3.4.0 (SDK oficial). `--mcp-config`/`--strict-mcp-config` de Claude, `codex mcp add` de Codex.
**Escribe:** `eva-mcp`, un binario stdio con 10 herramientas.

- Herramientas: `open_app`, `close_app`, `open_url`, `insert_text`, `get_active_window`, `get_selection`, `notify`, `speak`, `ask_user_confirmation`, `list_projects`.
- **Cableado sin tocar la configuración global del usuario**: EVA inyecta su servidor por invocación con `--mcp-config '{"eva":{…}}'`. Reversible, aislado.
- Aquí muere el nivel 1 para siempre: las órdenes compuestas las descompone el agente usando estas herramientas.
- **Seguridad:** el servidor MCP es una superficie de privilegio local. **Todas** las herramientas pasan por el gateway de la fase 5, y `ask_user_confirmation` es el único camino a lo sensible. Nada de `run_shell` entre las herramientas: para eso ya está el sandbox del propio agente (`codex sandbox`).

**Hecho cuando:** le pides a Codex que levante un servidor y él te abre el navegador en la URL correcta y te pide confirmación por el overlay.

---

### Fase 9 — Estilo, contexto y modo edición · semana 5-6

**Reutiliza:** el formateador Apple Intelligence ya encendido desde la fase 3, `frontmost` de la fase 5.
**Escribe:** prompts por app, selección de texto vía Accessibility API.

- Prompts por app ahora que hay frontmost: Slack casual, correo formal, **terminal sin puntuación**. Cada uno hereda la base del prompt de español de la fase 3, solo cambia el tono.
- Endpoint remoto compatible con OpenAI como alternativa **explícita** para cuando el on-device no alcance en calidad para una tarea puntual — nunca por defecto, porque los transcripts llevan secretos.
- Modo edición: seleccionas texto y dices "Adán, hazlo más formal". Se lee la selección con la Accessibility API (`AXUIElement`, vía `objc2` u otro binding equivalente) — mismo enfoque que Amical, escrito propio y acotado a esta única función.
- Contexto AX alrededor del cursor **solo si hace falta** más adelante, como pieza aislada — no antes.

**Hecho cuando:** la paridad con Wispr Flow está completa en Mac, con prompts distintos por app, sin que un solo transcript salga de la máquina por defecto.

---

### Fase 10 — Manos libres · por disparador, no por fecha

**Se abre cuando** te sorprendas buscando la tecla con las manos ocupadas. No antes: el prefijo tras hotkey tiene precisión perfecta por construcción, y un falso positivo aquí no escribe una palabra de más, **lanza un agente**.

- Etapa 2: captura siempre activa, escrita propia sobre `voice_activity_detector` (ya en el árbol desde la fase 2) + `nemotron-3.5-asr-streaming` (el único del catálogo con `streaming: true`, spd 84), para que la escucha continua no cueste batería.
- Etapa 3: wake word acústico con `ort` (ya dependencia transitiva de `transcribe-rs` con el feature `onnx`) cargando un modelo openWakeWord entrenado con voces Piper en español. Sin depender de ningún PR ajeno.
- **Puerta de entrada dura:** el asistente de calibración debe dar **0 falsos positivos en 2 minutos de tu habla libre** antes de que la función se pueda activar. Reusa el corpus de la fase 3.

---

## 5. Modelo por defecto: el shortlist con números

Verificado contra el catálogo de modelos que Handy publica (69 modelos, 25 con español explícito) — se usa como **referencia de qué modelos existen y sus scores**, no como dependencia: se cargan directo vía `transcribe-rs`.

| Modelo | acc | spd | Tamaño | Idiomas | Para qué |
|---|---|---|---|---|---|
| **canary-1b-flash** | 90 | 83 | 1B | en/de/es/fr | **Default propuesto**: el mejor equilibrio |
| canary-180m-flash | 88 | **98** | 180M | en/de/es/fr | Latencia mínima; el más rápido |
| cohere-transcribe-03-2026 | **92** | 63 | 2.0B | 14 | Cuando quieras precisión sobre velocidad |
| parakeet-tdt-0.6b-v3 | 88 | 79 | 0.6B | 25 | `lang_detect` + timestamps por token |
| nemotron-3.5-asr-streaming | 82 | 84 | 0.6B | 28 | **El único `streaming: true`** → fase 10 |
| whisper-large-v3-turbo | 88 | 35 | 809M | 100 | El más multilingüe, si hiciera falta |

Todos disponibles en `transcribe-rs` (Parakeet/Canary/Cohere vía el feature `onnx`; Whisper vía `whisper-cpp` con Metal en Apple Silicon). La fase 1 decide con **tus** audios, corriendo estos candidatos directo desde el crate.

---

## 6. Seguridad y privacidad

1. **Worktree por defecto** en tareas dictadas: lo peor que puede pasar es una rama desechable.
2. **Sandbox nativo**, no propio: `-s workspace-write` + `-C <proj>` en Codex; `--permission-mode` + `--add-dir` + `--allowedTools` en Claude. El gateway elige banderas.
3. **Nada destructivo por voz.** Confirmación por clic o hotkey; lista negra de verbos con homófonos del español antes de las reglas.
4. **Inyección por dictado**: si dictas en una terminal donde corre un agente, tu voz entra en su prompt. Con `frontmost` ya sabes el destino: "terminal con agente activo" se trata distinto de un editor.
5. **Los transcripts llevan secretos.** Apple Intelligence on-device por defecto; lo remoto se enciende a mano y con aviso.
6. **Captura siempre activa (fase 10) apagada por defecto**, con retención corta cuando se encienda.
7. **El servidor MCP es privilegio local**: todo pasa por el gateway, sin `run_shell` entre las herramientas.
8. **Notarización** en el CI desde la fase 2.
9. **Superficie de ataque nueva a vigilar**: al no forkear, no hay upstream que audite el bridging `objc2`/AppKit por ti. Los tres puntos sensibles (paste, overlay, hotkey) son código propio y deben tener tests propios desde la fase 2.

---

## 7. Métricas que bloquean release

| Número | Meta | Por qué |
|---|---|---|
| **p95** soltar tecla → texto pegado | < 1.200 ms (10 s de habla, Apple Silicon) | El p50 miente: lo que te saca de la herramienta es la cola. Sin el overhead de un WebView, esta meta debería ser más fácil de alcanzar que con un shell Tauri |
| **Retrabajos/día** (correcciones a mano) | < 5 | La única métrica que correlaciona con "lo sigo usando"; sale gratis de la hotkey de cosecha |
| **Comandos ejecutados ≠ pedidos** | 0 destructivos · < 1/día benignos | Un comando mal entendido cuesta mucho más que una palabra mal transcrita |

WER se mide como diagnóstico (¿modelo o diccionario?), no como puerta.

---

## 8. Lo que este plan decide NO construir

Fork de Handy (§0) · Tauri/React/bun para la UI · router LLM propio (§3.2) · sandbox propio (lo dan los CLIs) · orquestador de tareas propio (`claude agents` ya lo da) · sistema de plugins propio (MCP) · Ollama (Apple Intelligence) · catálogo de 69 modelos con UI de descarga (1-2 modelos fijados alcanzan) · escaneo de carpeta raíz en el MVP (el proyecto enfocado basta) · GUI de settings en el MVP (config + CLI alcanza) · soporte Windows antes de que alguien lo pida.

Cada línea de esa lista es tiempo, dependencias y superficie de mantenimiento que antes estaban en el plan.

---

## 9. Riesgos

| Riesgo | Prob. | Mitigación |
|---|---|---|
| **El despacho por voz resulta incómodo y no lo usas** | media | Es *la* razón del orden: lo sabes al final de la fase 6, no en la semana 7 |
| Un crash en el motor STT (ONNX/whisper.cpp) o en la FFI de Swift tumba la app | **era alta, ahora mitigado por diseño** | §3.3: vive en `eva-worker`, aislado de `eva-shell`; `eva-shell` lo reinicia solo. La prueba de `kill -9` es criterio de "hecho" en la fase 2, no una aspiración |
| Un bug se queda "escuchando"/"pensando" para siempre sin avisar | media, alto costo si ocurre | §3.3 punto 4: máquina de estados tipada + *watchdog* con timeout, criterio de "hecho" en la fase 7 |
| Los CLIs de agente cambian de bandera y rompen el adaptador | **alta** | Plantillas editables en config + versión "conocida-buena" comparada en cada arranque + smoke test automático si difiere (§3.4) + `doctor` de cada CLI |
| **Ahora dueño al 100 % de la capa nativa macOS**: sin fixes gratis de una comunidad externa | media | Trade-off central de §0, aceptado a cambio de simplicidad; mitigado con `catch_unwind` + tests propios en `eva-macos` + `cargo audit` en CI desde la fase 2 |
| El propio diseño de confiabilidad (2 procesos, IPC, watchdogs) es más código y más superficie que un binario simple | baja | Es deliberado: el patrón (shell supervisa worker) es estándar en apps de escritorio maduras, no una invención; se instala en la fase 2, no se añade después a más costo |
| `objc2`/AppKit tiene una curva de aprendizaje si es la primera vez | media | Se mide explícitamente en la fase 1 (spike de 40 líneas) antes de comprometerse |
| Windows queda genuinamente más lejos que con Tauri | media (si se necesita pronto) | Aceptado: el plan ya decía "Windows después"; ahora es una implementación paralela real, no un flag — se revisita si/cuando haga falta |
| Levenshtein/Jaro-Winkler sin fonética no basta para el diccionario en español | baja | Se mide en la fase 3 con el corpus propio; margen para añadir una regla fonética simple en español si hiciera falta, sin heredar Soundex |
| STT en español peor de lo esperado | media | Fase 1 con tus audios y 4-6 candidatos, directo sobre `transcribe-rs` |
| Permisos de macOS se pierden en cada build | alta | Firma estable desde la fase 2 |

---

## 10. Decisiones abiertas, con default para que nada bloquee

| # | Decisión | Default |
|---|---|---|
| 1 | Arquitectura: nativa vs. fork de Handy | **Nativa. Cerrada con esta iteración** (§0): las piezas difíciles de Handy son librerías sueltas; el resto es peso que no hace falta cargar |
| 2 | Modelo por defecto | **canary-1b-flash**, confirmado con tus audios en la fase 1, vía `transcribe-rs` directo |
| 3 | Agente por defecto | **Codex ▸ Claude Code**, autodetectado con `doctor`, forzable por voz |
| 4 | Palabra de activación | **"Adán"**, con accent-folding correcto desde el primer commit del gate (ya no hay parche que aplicar, se escribe bien de una vez) |
| 5 | Hotkey | **`fn` sostenida** vía `global-hotkey`, con `⌥ Space` como alternativa en config |
| 6 | Licencia y visibilidad | **MIT, repo privado, cuenta personal.** Pasar a Neobyte después es un `gh repo transfer` |
| 7 | Firma | Desarrollo local ahora; **Apple Developer ID** cuando quieras distribuir |
| 8 | Carpeta raíz de proyectos | **No hace falta para el MVP**: la fase 6 usa el proyecto enfocado |
| 9 | Settings: GUI o config | **Config + CLI para el MVP.** Una ventana nativa (`objc2-app-kit` o un `WKWebView` mínimo, sin el resto de Tauri) se evalúa después de la fase 9, si hace falta |
| 10 | Arquitectura de proceso: uno o dos | **Dos, desde la fase 2** (§3.3): `eva-shell` (tray/hotkey/overlay, casi nunca falla) supervisa a `eva-worker` (todo el código que puede fallar de verdad). Es la decisión que hace "ambicioso" y "resiliente" compatibles sin añadir complejidad de producto, solo de infraestructura interna |

---

## Apéndice — Arranque

```bash
mkdir eva01 && cd eva01
cargo init --name eva01
git init

# El workspace: 2 binarios + 7 crates
cat > Cargo.toml <<'EOF'
[workspace]
members = ["bins/*", "crates/*"]
resolver = "2"
EOF
mkdir -p bins/{eva-shell,eva-worker}
mkdir -p crates/{eva-audio,eva-text,eva-intent,eva-agents,eva-mcp,eva-macos,eva-store}

# Fase 1: el spike de STT, directo sobre la librería — sin CLI de terceros
cargo new eva-spike && cd eva-spike
cargo add transcribe-rs --features onnx,whisper-cpp
# cargar canary-1b-flash / cohere-transcribe / whisper-large-v3-turbo contra tus 10 audios

# Fase 1: el spike de plataforma — tray + hotkey + overlay, sin Tauri
cargo add tray-icon global-hotkey objc2 objc2-app-kit

# Fase 2: el resto de dependencias del workspace, incluida la disciplina de confiabilidad (§3.3-3.4)
cargo add rmcp voice_activity_detector cpal rubato rusqlite strsim unicode-normalization notify-rust self_update
cargo add tracing tracing-subscriber thiserror
cargo add --dev proptest
cargo install cargo-audit   # una vez, para correrlo en CI y localmente

# Fase 2: el gate de calidad que no se negocia, desde el primer PR —
# la misma línea al tope del lib.rs de cada uno de los 7 crates
for c in eva-audio eva-text eva-intent eva-agents eva-mcp eva-macos eva-store; do
  mkdir -p "crates/$c/src"
  echo '#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]' > "crates/$c/src/lib.rs"
done

# Verificar en ESTA Mac que Apple Intelligence está disponible y en español
# (System Settings → Apple Intelligence & Siri → idioma español)
say -v '?' | grep es_          # confirma Mónica/Paulina para TTS (fase 7)
```

## Apéndice — Atribución

- `LICENSE` MIT (copyright EVA01).
- `THIRD_PARTY_NOTICES.md` para las dependencias de Cargo (generado con `cargo about` o similar) — no hace falta un `UPSTREAM.md` de fork porque no hay fork.
- Ideas de diseño observadas en Handy, OpenFlow, Flow y Amical (el gate de wake word, el patrón de "reliable paste", el contexto vía Accessibility API) se implementan propias; se documenta en el código qué proyecto inspiró cada decisión, sin copiar el archivo.
- De VoiceTypr, Tambourine, Whispering y VoiceInk (GPL/AGPL) no se copia ni una línea: solo se mira la UX.
