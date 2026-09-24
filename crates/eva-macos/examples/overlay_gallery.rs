//! Draws the overlay in each state it can be in and writes one PNG per state,
//! so the layout can be looked at (text that wraps, four-line questions…)
//! instead of trusted. Run: `cargo run -p eva-macos --example overlay_gallery -- <out dir>`.

use eva_macos::{Activity, Choice, Icon, Overlay, OverlayContent, Tone};
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
        ("pregunta", "¿Cerrar Spotify?\nlo pide un agente por las herramientas de EVA", Tone::Ask, Activity::None),
    ];
    let with_icons = [
        ("abriendo_spotify", "Abriendo Spotify", Icon::App("Spotify".to_string())),
        ("abriendo_brave", "Abriendo Brave Browser", Icon::App("Brave Browser".to_string())),
        ("cerrando_finder", "Cerrando Notes", Icon::App("Notes".to_string())),
        ("abriendo_pagina", "Abriendo github.com", Icon::Symbol("globe")),
        ("buscando", "Buscando recetas de pasta", Icon::Symbol("magnifyingglass")),
        ("agente", "Agente trabajando", Icon::Symbol("sparkles")),
        ("app_abierta", "Spotify abierto", Icon::App("Spotify".to_string())),
    ];
    let mut shots: Vec<(&str, OverlayContent)> = states
        .into_iter()
        .map(|(name, text, tone, activity)| {
            (name, OverlayContent { text: text.to_string(), tone, activity, icon: Icon::None, choices: Vec::new() })
        })
        .collect();
    shots.extend(with_icons.into_iter().map(|(name, text, icon)| {
        let tone = if name == "app_abierta" { Tone::Ok } else { Tone::Neutral };
        let activity = if name == "app_abierta" { Activity::None } else { Activity::Executing };
        (name, OverlayContent { text: text.to_string(), tone, activity, icon, choices: Vec::new() })
    }));
    for (name, content) in &mut shots {
        if *name == "pregunta" {
            let choice = |label: &str, shortcut: &str, primary| Choice {
                label: label.to_string(),
                shortcut: shortcut.to_string(),
                primary,
            };
            content.choices = vec![choice("Sí", "⌘⏎", true), choice("No", "⌘⎋", false)];
        }
    }
    for (name, content) in shots {
        overlay.show(&content);
        overlay.settle(0.6);
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

    // The buttons take real clicks: send the app a mouse-down over each.
    let ask = OverlayContent {
        text: "¿Cerrar Spotify?\nlo pide un agente".to_string(),
        tone: Tone::Ask,
        activity: Activity::None,
        icon: Icon::None,
        choices: vec![
            Choice { label: "Sí".into(), shortcut: "⌘⏎".into(), primary: true },
            Choice { label: "No".into(), shortcut: "⌘⎋".into(), primary: false },
        ],
    };
    overlay.show(&ask);
    NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.2));
    let window = app.windows().iter().find(|w| w.isVisible()).expect("the island is on screen");
    for (name, x) in [("sí", 90.0), ("no", 250.0)] {
        let event = objc2_app_kit::NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            objc2_app_kit::NSEventType::LeftMouseDown,
            objc2_foundation::NSPoint::new(x, 25.0),
            objc2_app_kit::NSEventModifierFlags::empty(),
            0.0,
            window.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .expect("a mouse event");
        app.sendEvent(&event);
        println!("clic en {name}: {:?}", overlay.take_choice());
    }
}
