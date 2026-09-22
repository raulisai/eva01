# EVA01 — Disciplina de ingeniería

Reglas que este repositorio hace cumplir, no que pide de memoria. Si una regla
de aquí y el código no coinciden, el código está mal — se corrige el código,
no la regla, salvo que se discuta y se actualice este archivo explícitamente.

## 1. Un crate entra al workspace cuando está terminado, nunca antes

`Cargo.toml` (raíz) solo lista crates completos: compilan, tienen sus tests, y
`cargo clippy` pasa limpio. Un crate a medias no vive en `[workspace] members`
— vive fuera del workspace hasta que esté listo. Esto es deliberado: significa
que `cargo build --workspace` y `cargo test --workspace` **siempre** deben
funcionar en cualquier commit de `main`, sin excepción "es que fulano está a
medias".

## 2. Nada de `unwrap`/`expect`/`panic!` fuera de tests

Cada crate de librería empieza con:

```rust
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
```

Todo error real es un `Result<T, E>` con un error tipado (`thiserror`). La
única excepción aceptable es un `#[allow(clippy::expect_used)]` puntual y
comentado, justo antes de una línea donde el `panic!` es matemáticamente
imposible (por ejemplo, tras comprobar `!s.is_empty()` una línea antes) — y
solo cuando escribirlo como `Result` de verdad no aportaría nada, no como
atajo para evitar pensar el caso de error.

## 3. Ninguna función a medias

Si un archivo se crea, cada función pública hace lo que su documentación dice
que hace — nunca `todo!()`, `unimplemented!()`, ni un cuerpo vacío que
"ya se completará". Si una pieza depende de algo que no está listo todavía
(un modelo descargado, un binario externo, una firma de código), se documenta
esa dependencia externa explícitamente en el doc del módulo, pero el código
Rust que sí se puede escribir y probar hoy, se escribe completo hoy.

## 4. Todo módulo con lógica no trivial lleva tests en el mismo archivo

`#[cfg(test)] mod tests { … }` al final del archivo, no en un directorio
`tests/` aparte salvo que sea una prueba de integración entre crates. La
prueba vive al lado del código que prueba porque así nadie la deja
desactualizada sin verlo.

## 5. El límite de una frontera insegura (FFI, AppKit, procesos externos) se
   marca con un `trait`

Cualquier pieza que hable con algo que no es Rust puro (Swift, un CLI externo,
AppKit) se esconde detrás de un `trait` pequeño, para que la lógica que lo usa
se pueda probar con un doble de prueba (mock) sin tocar el sistema real. Ver
`eva-text::formatter::Formatter` como ejemplo: la limpieza de texto se prueba
entera sin necesitar Apple Intelligence instalado.

## 6. CI, cuando exista, corre exactamente esto y nada pasa si algo falla

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo audit
```

## 7. Referencia

Las decisiones de arquitectura que motivan estas reglas —por qué dos procesos,
por qué nada de `unwrap`, por qué una máquina de estados tipada— están en
[`PLAN.md`](PLAN.md) §3.3-3.4. Este archivo es el "cómo se aplica todos los
días"; ese es el "por qué".
