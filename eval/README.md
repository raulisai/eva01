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

# Con tu diccionario personal aplicado (para que el WER refleje lo que
# de verdad vas a dictar, no una versión desnuda del pipeline)
cargo run --release -p eva-eval -- --corpus eval/corpus --word García --word Núñez
```

## Qué reporta

- **WER** (Word Error Rate) por muestra y promedio — diagnóstico, no
  bloquea un release por sí solo (`docs/PLAN.md` §7: la mediana miente,
  lo que saca de la herramienta es la cola).
- **Latencia p50/p95** de la transcripción — la meta es p95 < 1200ms
  (frase de 10s, Apple Silicon).
- **Muletillas que sobrevivieron**: cuántas muestras todavía muestran un
  relleno ambiguo ("este", "pues", "bueno"…) después de limpiar — la
  señal de que el formateador con contexto (fase 9, Apple Intelligence)
  hace falta, no un fallo del pipeline de reglas.

## Cómo cosechar muestras nuevas

1. Usa EVA01 normalmente.
2. Cuando algo salga mal, marca esa transcripción con la hotkey "esto
   salió mal" (o revisa `eva-store::transcripts_marked_bad()` / la tabla
   `transcripts` directamente).
3. Exporta el audio y el texto correcto a un par `.wav`/`.txt` aquí.

No hay una meta de tamaño fija — el corpus crece con lo que realmente te
falló, que es la parte que importa medir.
