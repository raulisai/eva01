//! Draws the overlay in each state it can be in and writes one PNG per state,
//! so the layout can be looked at (text that wraps, four-line questions…)
//! instead of trusted. Run: `cargo run -p eva-macos --example overlay_gallery -- <out dir>`.

use eva_macos::{Activity, Overlay, OverlayContent, Tone};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSDate, NSRunLoop};

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".to_string()));
    std::fs::create_dir_all(&out).expect("output directory");
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let overlay = Overlay::new(mtm);

    let states = [
        ("escuchando", "Escuchando", Tone::Neutral, Activity::Listening),
        ("pensando", "Pensando", Tone::Neutral, Activity::Thinking),
        ("ejecutando", "Ejecutando", Tone::Neutral, Activity::Executing),
        ("listo", "✓ Listo", Tone::Ok, Activity::None),
        (
            "error_largo",
            "✗ hay un campo de contraseña activo y macOS no deja pegar aquí; el texto quedó en el portapapeles",
            Tone::Error,
            Activity::None,
        ),
        (
            "tarea",
            "✓ Tarea lista: actualicé la documentación, corregí tres pruebas que fallaban y abrí la rama eva/3fa9c1d2",
            Tone::Ok,
            Activity::None,
        ),
        (
            "pregunta",
            "¿Cerrar Spotify?\nlo pide un agente por las herramientas de EVA\n⌘⏎ sí · ⌘⎋ no · 28 s",
            Tone::Ask,
            Activity::None,
        ),
    ];
    for (name, text, tone, activity) in states {
        overlay.show(&OverlayContent { text: text.to_string(), tone, activity });
        overlay.animate(0.6);
        // Let AppKit lay the view out before drawing it.
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.25));
        match overlay.snapshot_png() {
            Some(png) => {
                std::fs::write(out.join(format!("{name}.png")), png).expect("write png");
                println!("{name}.png");
            }
            None => eprintln!("{name}: no se pudo dibujar"),
        }
    }
}
