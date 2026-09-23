//! `eva startup`: opening EVA01 when you log in, through a per-user
//! LaunchAgent — no helper tool, no admin rights, and undone by deleting one
//! file.

use std::path::{Path, PathBuf};

const LABEL: &str = "dev.eva01.app";

/// The LaunchAgent file for the current user.
fn agent_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

/// The `EVA01.app` bundle this `eva` binary lives in
/// (`…/EVA01.app/Contents/MacOS/eva`), if it does.
fn find_app_bundle(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find(|p| p.extension().is_some_and(|ext| ext == "app")).map(Path::to_path_buf)
}

/// The LaunchAgent that opens `app` at login. `open -a` rather than running
/// the binary directly, so macOS treats it as the app it is: its permissions,
/// its icon, no Terminal window.
fn plist(app: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/bin/open</string>
        <string>-a</string>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
        xml_escape(&app.to_string_lossy())
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn user_id() -> Option<String> {
    let output = std::process::Command::new("id").arg("-u").output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Whether EVA01 opens at login.
pub fn is_enabled() -> bool {
    agent_path().is_some_and(|p| p.is_file())
}

/// `eva startup enable`.
pub fn enable() -> i32 {
    let Some(path) = agent_path() else {
        eprintln!("no se encontró tu carpeta personal");
        return 2;
    };
    let Some(app) = std::env::current_exe().ok().and_then(|exe| find_app_bundle(&exe)) else {
        eprintln!("`eva` no está dentro de EVA01.app. Instala la app (packaging/build-app.sh) y usa el `eva` de dentro del paquete.");
        return 1;
    };

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("no se pudo crear {}: {e}", parent.display());
            return 1;
        }
    }
    if let Err(e) = std::fs::write(&path, plist(&app)) {
        eprintln!("no se pudo escribir {}: {e}", path.display());
        return 1;
    }
    // Loads it now too, so it does not wait for the next login to exist.
    // Already-loaded is not a problem.
    if let Some(uid) = user_id() {
        let _ = std::process::Command::new("launchctl").args(["bootstrap", &format!("gui/{uid}")]).arg(&path).output();
    }
    println!("EVA01 se abrirá al iniciar sesión ({}).", path.display());
    0
}

/// `eva startup disable`.
pub fn disable() -> i32 {
    let Some(path) = agent_path() else { return 2 };
    if let Some(uid) = user_id() {
        let _ = std::process::Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{LABEL}")]).output();
    }
    match std::fs::remove_file(&path) {
        Ok(()) => println!("EVA01 ya no se abre al iniciar sesión."),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("EVA01 no estaba configurado para abrirse al iniciar sesión."),
        Err(e) => {
            eprintln!("no se pudo borrar {}: {e}", path.display());
            return 1;
        }
    }
    0
}

/// `eva startup status`.
pub fn status() -> i32 {
    println!("{}", if is_enabled() { "EVA01 se abre al iniciar sesión." } else { "EVA01 no se abre al iniciar sesión (`eva startup enable`)." });
    0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_found_from_the_binary_inside_it() {
        let exe = Path::new("/Applications/EVA01.app/Contents/MacOS/eva");
        assert_eq!(find_app_bundle(exe), Some(PathBuf::from("/Applications/EVA01.app")));
    }

    #[test]
    fn a_binary_outside_any_bundle_has_no_app() {
        assert_eq!(find_app_bundle(Path::new("/Users/x/code/eva01/target/debug/eva")), None);
    }

    #[test]
    fn the_plist_opens_the_app_at_login_through_open() {
        let text = plist(Path::new("/Applications/EVA01.app"));
        assert!(text.contains("<string>dev.eva01.app</string>"));
        assert!(text.contains("<string>/usr/bin/open</string>"));
        assert!(text.contains("<string>/Applications/EVA01.app</string>"));
        assert!(text.contains("<key>RunAtLoad</key>\n    <true/>"));
    }

    #[test]
    fn a_path_with_xml_characters_cannot_break_out_of_the_plist() {
        let text = plist(Path::new("/Apps/A&B <x>.app"));
        assert!(text.contains("A&amp;B &lt;x&gt;.app"));
        assert!(!text.contains("<x>"));
    }

    #[test]
    fn the_generated_plist_is_valid_for_plutil() {
        let path = std::env::temp_dir().join(format!("eva-plist-test-{}.plist", std::process::id()));
        std::fs::write(&path, plist(Path::new("/Applications/EVA01.app"))).unwrap();
        let status = std::process::Command::new("plutil").arg("-lint").arg(&path).output().expect("plutil ships with macOS");
        let _ = std::fs::remove_file(&path);
        assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stdout));
    }
}
