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

Línea base medida (canary-1b-flash int8 + canary-180m-flash como segunda opinión, MacBook Air
M-series, macOS 26; 12 frases cortas y un dictado de 70 s):

| | WER | voz p50/p95 (frases) | voz+formato p50/p95 (frases) | dictado de 70 s |
|---|---|---|---|---|
| solo reglas | 5,1 % | 261 / 373 ms | igual (formato < 2 ms) | ~7 s de espera |
| Apple Intelligence | 5,1 % | ~280 / ~370 ms | 1000 / 1169 ms | 14,2 s de espera |

La segunda opinión corre en paralelo en las frases cortas y les suma ~60 ms de voz; sin ella la voz
es ~200 ms de p50. Corpus de formato (40 frases, Apple Intelligence, decodificación codiciosa):
40/40 aceptadas, mediana 706 ms. (Con `--padding silence`, el comportamiento anterior, el WER limpio
es 4,2 %; ver «Ruido de fondo» para por qué ya no es el predeterminado.)

Lo que enseñó (y quedó corregido o anotado):

- Sin silencio previo, Canary **se comía la primera palabra** ("Hay que
  actualizar…" → "Que actualizar…"). Añadir 300 ms de silencio antes y 200 después
  lo arregló en este corpus (6,6 % → 4,2 %)… hasta que se probó con ruido (abajo):
  ese relleno es lo que peor lleva el modelo cuando hay ruido de fondo.
- La primera llamada a Apple Intelligence tarda ~2 s (carga del modelo) y
  las siguientes ~0,7 s: el worker la calienta al arrancar.
- El formateo es ~70 % de la latencia total con Apple Intelligence. Con
  p95 ≈ 1,24 s en 12 muestras, la meta de 1,2 s queda al borde: es el
  siguiente número a bajar, no algo resuelto.
- La única frase que falla es la de 0,5 s ("Ya voy"): con audios tan
  cortos el modelo alucina, con o sin relleno.

## Ruido de fondo

Las mismas 12 frases con ruido añadido (blanco a 40 y 30 dB por debajo de la voz; ruido grave de
ventilador a 20 y 10 dB; blanco a 20 dB). WER medio:

| relleno de la grabación | limpio | blanco −40 dB | blanco −30 dB | ventilador −20 dB | ventilador −10 dB | blanco −20 dB |
|---|---|---|---|---|---|---|
| silencio digital siempre | 4,2 % | 25 % | 47 % | 27 % | **154 %** | 77 % |
| ninguno | 6,6 % | 3,3 % | 5,8 % | 5,0 % | 9,7 % | 25 % |
| **`auto` (el actual)** | 5,6 % | 3,3 % | 5,8 % | 5,0 % | 9,7 % | 25 % |

Un tramo de ceros exactos junto a una grabación con ruido hace tropezar al modelo (repite «me estoy me
estoy…», inventa frases). `auto` solo rodea de silencio digital la grabación cuya propia base es silenciosa
(el fragmento más callado por debajo de 0,001 de RMS) y deja las demás como vienen. También se probó rellenar con
ruido del mismo nivel que la propia grabación: pierde contra no rellenar en todo menos en audio limpio. El
ajuste `[stt] padding` (`auto` / `silence` / `none`) y `eva-eval --padding` permiten repetir la comparación
con **tus** dictados marcados: el ruido de estas pruebas es sintético y el de tu micrófono no lo es.

## Órdenes cortas y la palabra de activación

Órdenes dichas por `say` («Adán, abre Brave», «Adán, cierra Spotify»…, 1–2,5 s, dos voces) pasadas
por la compuerta real de EVA01 (`eva intent`):

| | órdenes reconocidas |
|---|---|
| solo `canary-1b-flash` | **4 / 22** — «Adán» sale «Y luego…», «Ah, entonces…» |
| `canary-1b-flash` + `canary-180m-flash` como segunda opinión | **19 / 22** |

El modelo grande, que es el mejor dictando frases (5 % de WER contra 19 % del pequeño, que además
pierde la ñ: «ma ana», «Espa a»), es el peor en clips de 1–2 s; el pequeño reconoce «Adán» 7 de 8
veces. `eva_audio::second_opinion` usa el grande y, solo si el clip es corto y su texto no empieza
con la palabra de activación, le pregunta al pequeño y toma su versión únicamente si *esa* sí empieza
con ella. El dictado normal no cambia (mismo 5,1 % en el corpus). Cuesta ~250 MB de memoria.

También se probó qué palabra de activación entiende mejor el modelo grande solo (de 8): «Asistente»
8, «Computadora» 7, «Mercurio» 6, «Eva» 4, «Adán» 2, «Oye Adán» y «Oye Eva» 0. Cambiarla es una
decisión de producto (`eva wake-word`), no un arreglo: con la segunda opinión «Adán» ya funciona.

En clips cortos el modelo grande además puede entrar en **bucle** («Computa, bueno, bueno, bueno…»
decenas de veces). Eso nunca se pega: una repetición de 4+ palabras (o 3+ pares) se colapsa, y
también dispara la segunda opinión.

```bash
EVA_CANARY_MODEL_DIR=… cargo run --release -p eva-eval -- --corpus <órdenes> --raw \
  --second-opinion ~/Library/Application\ Support/EVA01/models/canary-180m-flash
```

## Frases de una o dos palabras

28 clips de `say` de 0,2–1,2 s («sí», «gracias», «vale», «de acuerdo»…) con `canary-1b-flash`. Solos, el
modelo se calla (nada que pegar) o inventa («Vale» → «Ballet», «De acuerdo» → «Y ser como, camino a ir»): 1 de
28 exacto con el relleno por defecto, 11 con silencio digital alrededor. Más silencio no lo arregla.

Lo que sí ayuda es **decirlo tres veces seguidas en un solo audio** (con 350 ms de pausa) y dejar votar a las
repeticiones (`eva-audio::short_reply`, para clips de hasta 1,5 s): 14 de 28 exactos con el relleno por defecto, con
o sin segunda opinión. Con 2 repeticiones salen 8, con 4 salen 15 y con 5 bajan a 12, así que son 3. El resto sigue
mal («Balle», «Andirian» por «anterior»); con dos o tres palabras ya era fiable. Sigue siendo audio sintético: si
dictas muchas respuestas de una palabra, este es el caso a medir con tu propia voz.

## Dictados largos

Un minuto de dictado es uso normal, y era lo que peor funcionaba. Voz continua de `say`
(Paulina), `canary-1b-flash`, solo reglas:

| Duración | Antes (una sola pasada) | Ahora (cortado en las pausas) |
|---|---|---|
| 16 s · 25 s | 3,8 % · 0 % WER | igual (cabe entero) |
| 91 s | **110 %** WER, 29 s de espera | **0 %**, 8,6 s |
| 227 s | **93 %**, 96 s | **0 %**, 21 s |

Canary Flash rinde hasta ~25 s por pasada; más allá inventa, se calla o se pone lentísimo
(el coste crece más rápido que el audio). `CanarySpeechToText` corta el audio en el momento
más callado cerca de los 20 s (`eva-audio::segment`) y une los textos.

El formateo tenía el mismo problema con textos largos: con 120–190 palabras Apple Intelligence
cambia palabras («mándamelo» → «mándame») y la guarda rechaza *todo*, dejando el texto sin
puntuar. Ahora un dictado de más de 40 palabras se formatea en trozos de ≤ 30 (cortados donde el
modelo de voz ya puso un punto, o en una coma o antes de «porque», «y», «así»…), y un trozo que falla
solo se cae a sí mismo. Un dictado de 70 s de párrafos distintos: 6,6 s de voz + 8,3 s de formato,
WER 0,5 %, con puntuación completa. (El modelo del equipo no formatea trozos en paralelo: se probó y
no gana tiempo.)

### La espera de después de soltar la tecla

EVA01 no espera a que sueltes la tecla para trabajar: corta lo ya grabado en pausas, transcribe cada trozo y
formatea el texto por adelantado. `eva-eval --streaming` mide solo lo que queda tras soltar (la cola de audio y la
última frase). Con el dictado sintético de 70 s y Apple Intelligence: **14,9 s → 3,2 s** (voz 6,6 s → 1,4 s,
formato 8,3 s → 1,9 s), con el mismo texto salvo puntuación.

## Modo entrenamiento: tu voz, no `say`

Todas las cifras de arriba salen de voces sintéticas. Para medir cómo te oye **a ti**, activa el
«Modo entrenamiento» en el panel (página «Revisar»): cada dictado y orden guarda su audio y lo que se oyó y se
pegó en `~/Library/Application Support/EVA01/training/` (solo en tu Mac; nada si hay un campo de contraseña).
En «Revisar» escuchas cada uno y escribes lo que dijiste (o pulsas «Estaba bien»): eso crea el `.txt` de
referencia. Lo sin revisar se borra a los `[history] keep_days`; lo revisado es el corpus y se queda.

```bash
EVA_CANARY_MODEL_DIR=… cargo run --release -p eva-eval -- --corpus ~/Library/Application\ Support/EVA01/training \
  --raw --wake-word Eva --second-opinion ~/Library/Application\ Support/EVA01/models/canary-180m-flash
```

Con unas decenas de revisados se compara el modelo actual con las alternativas (mismo comando, otro
`EVA_CANARY_MODEL_DIR` o `EVA_STT_MODEL_PATH`), con datos tuyos.

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

## El pulido con el modelo local

`eva_text::polish` le da a cada dictado una segunda pasada con el modelo de
Ollama de `[resolver]`. El modelo solo propone; `polish::accept` se queda con
los términos técnicos que suenan como lo dicho, los ecos y las erratas
cercanas, y descarta todo lo demás. Para ver qué cambiaría en tus dictados
reales y cuánto tarda:

```bash
sqlite3 ~/Library/Application\ Support/EVA01/eva.sqlite3 \
    "select replace(raw, char(10), ' ') from transcripts where raw != ''" > /tmp/raws.txt
cargo run --release -p eva-text --example polish_probe -- /tmp/raws.txt qwen2.5:3b
```

Medido el 25 sep 2026 con `qwen2.5:3b` sobre 194 dictados: 13 cambiados, 0 a
peor tras endurecer el guardián, 0,76 s de media.
