#!/bin/sh
# Genera un corpus SINTÉTICO de arranque en eval/audio/ con las voces en
# español que trae macOS (`say`): frases cotidianas con ñ, acentos, cifras y
# nombres, una muy corta y un dictado continuo de ~70 s (el caso que más
# costó: ver eval/README.md), más una referencia por audio. Sirve para medir el modelo y la
# latencia en ESTE equipo sin grabar nada — no reemplaza tu voz real: el
# corpus de verdad se cosecha con el uso (eval/README.md). Está en .gitignore
# (eval/audio/), se regenera cuando quieras.
#
# Uso: eval/generate-synthetic.sh && cargo run --release -p eva-eval -- --corpus eval/audio

set -eu
OUT="$(cd "$(dirname "$0")" && pwd)/audio"
mkdir -p "$OUT"

# nombre|voz|frase (la referencia es la frase tal cual)
while IFS='|' read -r name voice text; do
    [ -z "$name" ] && continue
    say -v "$voice" -o "$OUT/$name.wav" --file-format=WAVE --data-format=LEI16@16000 -- "$text"
    printf '%s\n' "$text" > "$OUT/$name.txt"
done <<'CORPUS'
saludo|Mónica|Hola, buenos días a todos
abrir_navegador|Mónica|Quiero abrir el navegador y buscar el clima de mañana
enye_manana|Paulina|El clima de mañana en España será muy bueno
enye_senor|Mónica|El señor Núñez llegó por la mañana con su niño
numeros|Paulina|La reunión es el quince de marzo a las diez y media
dinero|Mónica|Son doscientos cincuenta dólares por mes
tecnico|Paulina|Hay que actualizar la documentación antes del viernes
correo|Mónica|Por favor confirma si recibiste mi mensaje anterior
tarea_larga|Paulina|El cliente pidió que agreguemos un botón de exportar en la pantalla de reportes
pregunta|Mónica|Cuándo vas a llegar a la oficina
nombres|Paulina|Mándale el archivo a Juan García pero antes pregúntale si ya llegó
corta|Mónica|Ya voy
largo_70s|Paulina|Necesito que revises el informe de ventas del tercer trimestre y me digas si los números coinciden con lo que presentó el equipo de finanzas la semana pasada, porque el director quiere tener todo claro antes de la reunión del lunes por la mañana. Si encuentras diferencias, anótalas en un documento aparte y mándamelo por correo antes del viernes al mediodía, por favor. Después quiero que agendes una llamada con el cliente de Guadalajara para hablar del contrato nuevo, porque todavía faltan por definir los plazos de entrega y el precio de las licencias adicionales. Recuérdame también llevar la presentación impresa y una copia del acuerdo anterior, por si hay que compararlos durante la conversación. Por otro lado, el equipo de desarrollo terminó la versión nueva de la aplicación móvil y ya está lista para pruebas, así que te pido que coordines con Marta para que la revisen antes de publicarla en las tiendas. Finalmente, no olvides reservar la sala grande para el jueves en la tarde, porque vienen tres personas de otra oficina y necesitamos espacio para trabajar con calma. Muchas gracias por todo, y avísame si algo no queda claro.
CORPUS
echo "Corpus sintético en $OUT ($(ls "$OUT"/*.wav | wc -l | tr -d ' ') audios)"
