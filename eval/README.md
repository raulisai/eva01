# El corpus de EVA01

`docs/PLAN.md` fase 3: este corpus se **cosecha del uso real**, no se
escribe a mano — la fuente principal son tus propias grabaciones (vía la
hotkey "esto salió mal" y las transcripciones que `eva-store` ya guarda),
no una sesión dedicada de grabar 100 frases.

## Formato

Cada muestra es un par de archivos con el mismo nombre:

```
eval/corpus/
├── adan_abre_brave.wav     16 kHz, mono, WAV
├── adan_abre_brave.txt     la transcripción de referencia, tal como
│                           debería salir del pipeline completo (ya
│                           limpia: con mayúsculas, puntuación, sin
│                           muletillas evitables)
├── dictado_largo.wav
├── dictado_largo.txt
└── ...
```

Un `.wav` sin su `.txt` correspondiente se omite con un aviso — así el
corpus puede crecer de a poco (grabas antes de escribir la referencia)
sin que `eva-eval` falle.

## Cómo correrlo

```bash
# Con Canary (el modelo de producción, docs/PLAN.md §5)
EVA_CANARY_MODEL_DIR=/ruta/a/canary-1b-flash cargo run --release -p eva-eval -- --corpus eval/corpus

# O con Whisper
EVA_STT_MODEL_PATH=/ruta/a/ggml-base.bin cargo run --release -p eva-eval -- --corpus eval/corpus

# Con el formateador real (Apple Intelligence) en vez de solo reglas
cargo run --release -p eva-eval -- --corpus eval/corpus --apple-intelligence

# Con tu diccionario personal aplicado (para que el WER refleje lo que
# de verdad vas a dictar, no una versión desnuda del pipeline)
cargo run --release -p eva-eval -- --corpus eval/corpus --word García --word Núñez
```

## Qué reporta

- **WER** (Word Error Rate) por muestra y promedio — diagnóstico, no
  bloquea un release por sí solo (`docs/PLAN.md` §7: la mediana miente,
  lo que saca de la herramienta es la cola). Por defecto se calcula sin
  mayúsculas ni puntuación (eso lo juzga el formateador, no el modelo de
  voz); `--strict` las cuenta también. Los acentos y la ñ **sí** cuentan.
- **Latencia p50/p95**, separada: voz (transcripción), y voz+formato (lo
  que espera quien dicta) — la meta es p95 < 1200ms (frase de 10s,
  Apple Silicon), sin contar el pegado. El eval calienta el formateador
  antes de medir, igual que el worker al arrancar.
- **Muletillas que sobrevivieron**: cuántas muestras todavía muestran un
  relleno ambiguo ("este", "pues", "bueno"…) después de limpiar — la
  señal de que el formateador con contexto (fase 9, Apple Intelligence)
  hace falta, no un fallo del pipeline de reglas.

## Corpus sintético de arranque

Mientras no haya grabaciones tuyas, `eval/generate-synthetic.sh` genera 12
frases con las voces en español de macOS (`say`) en `eval/audio/`
(ignorado por git). Mide el modelo y la latencia en este equipo, **no tu
voz**: un modelo entrenado con voz humana lo suele oír mejor que a ti,
así que trátalo como piso de humo, no como precisión real.

```bash
eval/generate-synthetic.sh
EVA_CANARY_MODEL_DIR=~/Library/Application\ Support/EVA01/models/canary-1b-flash \
  cargo run --release -p eva-eval -- --corpus eval/audio --apple-intelligence
```

Línea base medida (canary-1b-flash int8, M-series, macOS 26, 12 frases):

| | WER | voz p50/p95 | voz+formato p50/p95 |
|---|---|---|---|
| solo reglas | 4,2 % (11/12 perfectas) | 243 / 426 ms | igual (formato < 2 ms) |
| Apple Intelligence | 4,2 % | 247 / 413 ms | 968 / 1238 ms |

Lo que enseñó (y quedó corregido o anotado):

- Sin silencio previo, Canary **se comía la primera palabra** ("Hay que
  actualizar…" → "Que actualizar…"). `CanarySpeechToText` ahora añade
  300 ms de silencio antes y 200 ms después; el WER bajó de 6,6 % a 4,2 %.
- La primera llamada a Apple Intelligence tarda ~2 s (carga del modelo) y
  las siguientes ~0,7 s: el worker la calienta al arrancar.
- El formateo es ~70 % de la latencia total con Apple Intelligence. Con
  p95 ≈ 1,24 s en 12 muestras, la meta de 1,2 s queda al borde: es el
  siguiente número a bajar, no algo resuelto.
- La única frase que falla es la de 0,5 s ("Ya voy"): con audios tan
  cortos el modelo alucina, con o sin relleno.

## Cómo cosechar muestras nuevas

1. Usa EVA01 normalmente.
2. Cuando un dictado (o una orden) salga mal, márcalo: **⌃⌥⌘M** o «Esto
   salió mal (último dictado)» en el menú de la barra. EVA01 guarda el
   audio y el texto de *ese* momento; antes de marcar, no se escribe nada
   de lo que dices (el audio del último enunciado solo vive en memoria, y
   uno dictado en un campo de contraseña ni eso).
3. Mira lo marcado con `eva history --flagged`. Los archivos están en
   `~/Library/Application Support/EVA01/harvest/`:

   ```
   <id>.wav            el audio, 16 kHz mono
   <id>.propuesta.txt  lo que oyó el modelo y lo que se pegó
   ```

4. La referencia es **lo que de verdad dijiste**, y solo tú la sabes: crea
   `<id>.txt` con esa frase, tal como debería haber salido, y mueve el par
   `.wav`/`.txt` a `eval/corpus/`. (Un `.wav` sin `.txt` se omite, así que
   lo que aún no corriges no falsea el WER.)

El número que sale gratis de esto es el de **retrabajos**: `eva history`
cuenta los marcados de las últimas 24 h (la meta de `docs/PLAN.md` §7 es
menos de 5 al día).

No hay una meta de tamaño fija — el corpus crece con lo que realmente te
falló, que es la parte que importa medir. El historial de texto se guarda en
este equipo 30 días (`[history]` en la configuración; los marcados no se
borran) y se puede apagar del todo con `save_transcripts = false`.
