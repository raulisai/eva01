//! Music control for Spotify and Apple Music: play a playlist, resume, pause,
//! next, previous. Both apps expose these through AppleScript and nothing
//! else does (no public API), so this runs `osascript` with a script built
//! here — never from free text: a target is checked against the shapes it can
//! have (a Spotify link, a playlist's name) and escaped before it goes in.
//!
//! The first use of each app asks macOS for Automation permission ("EVA01
//! quiere controlar Spotify"); until it is granted the error says so.

use crate::error::MacosError;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What to do with the music player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaCommand {
    /// Play: a Spotify link (`spotify:playlist:…`, `open.spotify.com/…`) or the
    /// name of an Apple Music playlist; `None` resumes what was playing.
    Play(Option<String>),
    /// Pause.
    Pause,
    /// The next track.
    Next,
    /// The previous track.
    Previous,
}

impl MediaCommand {
    /// A short description for the confirmation prompt and the audit trail.
    pub fn describe(&self) -> String {
        match self {
            MediaCommand::Play(Some(target)) => format!("reproducir {target}"),
            MediaCommand::Play(None) => "reanudar la música".to_string(),
            MediaCommand::Pause => "pausar la música".to_string(),
            MediaCommand::Next => "siguiente canción".to_string(),
            MediaCommand::Previous => "canción anterior".to_string(),
        }
    }
}

/// The two players it can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Player {
    Spotify,
    Music,
}

impl Player {
    fn app(self) -> &'static str {
        match self {
            Player::Spotify => "Spotify",
            Player::Music => "Music",
        }
    }
}

/// What a `Play` target is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// A canonical `spotify:<kind>:<id>` link.
    Spotify(String),
    /// The name of a playlist in Apple Music.
    Playlist(String),
}

const SPOTIFY_KINDS: [&str; 6] = ["playlist", "album", "track", "artist", "show", "episode"];

fn is_spotify_id(id: &str) -> bool {
    (10..=32).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// What `text` asks to play, or why it cannot be trusted into a script.
fn target_of(text: &str) -> Result<Target, MacosError> {
    let text = text.trim();
    let bad = |why: &str| MacosError::MediaFailed(format!("«{text}» no se puede reproducir: {why}"));
    if text.is_empty() || text.chars().any(char::is_control) || text.chars().count() > 120 {
        return Err(bad("no es un enlace de Spotify ni el nombre de una playlist"));
    }
    if let Some(rest) = text.strip_prefix("spotify:") {
        let mut parts = rest.split(':');
        return match (parts.next(), parts.next(), parts.next()) {
            (Some(kind), Some(id), None) if SPOTIFY_KINDS.contains(&kind) && is_spotify_id(id) => {
                Ok(Target::Spotify(format!("spotify:{kind}:{id}")))
            }
            _ => Err(bad("el enlace de Spotify no tiene la forma spotify:playlist:…")),
        };
    }
    if let Some(rest) = text.strip_prefix("https://open.spotify.com/") {
        let path = rest.split(['?', '#']).next().unwrap_or("");
        let mut parts = path.split('/').filter(|p| !p.is_empty()).skip_while(|p| p.starts_with("intl-"));
        return match (parts.next(), parts.next()) {
            (Some(kind), Some(id)) if SPOTIFY_KINDS.contains(&kind) && is_spotify_id(id) => {
                Ok(Target::Spotify(format!("spotify:{kind}:{id}")))
            }
            _ => Err(bad("el enlace de Spotify no es de una playlist, álbum o canción")),
        };
    }
    Ok(Target::Playlist(text.to_string()))
}

/// `text` inside an AppleScript string literal.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The AppleScript that does `command` with `player`. Pure, so it is tested
/// without touching any app.
fn script_for(command: &MediaCommand, player: Player, target: Option<&Target>) -> String {
    let app = player.app();
    let body = match (command, target) {
        (MediaCommand::Play(_), Some(Target::Spotify(uri))) => format!("play track {}", quoted(uri)),
        (MediaCommand::Play(_), Some(Target::Playlist(name))) => format!("play playlist {}", quoted(name)),
        (MediaCommand::Play(_), None) => "play".to_string(),
        (MediaCommand::Pause, _) => "pause".to_string(),
        (MediaCommand::Next, _) => "next track".to_string(),
        (MediaCommand::Previous, _) => "previous track".to_string(),
    };
    format!("tell application \"{app}\" to {body}")
}

fn is_running(app: &str) -> bool {
    crate::workspace::is_app_running(app)
}

/// Which player to drive, and what it should play.
fn plan(command: &MediaCommand) -> Result<(Player, Option<Target>), MacosError> {
    if let MediaCommand::Play(Some(text)) = command {
        let target = target_of(text)?;
        let player = if matches!(target, Target::Spotify(_)) { Player::Spotify } else { Player::Music };
        return Ok((player, Some(target)));
    }
    // Pause, next, resume: whichever is open, Spotify first.
    let player = [Player::Spotify, Player::Music]
        .into_iter()
        .find(|p| is_running(p.app()))
        .ok_or_else(|| MacosError::MediaFailed("no hay Spotify ni Música abiertas".to_string()))?;
    Ok((player, None))
}

/// Does `command`.
///
/// # Errors
/// [`MacosError::MediaFailed`] when the target is not something playable, no
/// player is open, or the app refuses (with the macOS permission problem said
/// in plain words).
pub fn run(command: &MediaCommand) -> Result<(), MacosError> {
    let (player, target) = plan(command)?;
    if !is_running(player.app()) {
        // Only playing needs the app to be there; it is opened first.
        crate::workspace::open_app(player.app())?;
        let started = Instant::now();
        while !is_running(player.app()) && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(250));
        }
        std::thread::sleep(Duration::from_millis(2_500)); // it answers Apple events a moment after it exists
    }
    osascript(&script_for(command, player, target.as_ref()), player.app())
}

fn osascript(script: &str, app: &str) -> Result<(), MacosError> {
    let mut child = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| MacosError::MediaFailed(format!("no se pudo ejecutar osascript: {e}")))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut message = String::new();
                if let Some(mut err) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut err, &mut message);
                }
                return Err(MacosError::MediaFailed(explain(&message, app)));
            }
            Ok(None) if started.elapsed() > Duration::from_secs(15) => {
                let _ = child.kill();
                return Err(MacosError::MediaFailed(format!("{app} no respondió a tiempo")));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(MacosError::MediaFailed(e.to_string())),
        }
    }
}

/// What osascript said, in words for the user.
fn explain(stderr: &str, app: &str) -> String {
    if stderr.contains("-1743") || stderr.contains("Not authorized") {
        format!("macOS no dejó a EVA01 controlar {app}: actívalo en Ajustes → Privacidad y seguridad → Automatización")
    } else if stderr.contains("-1728") || stderr.contains("Can't get playlist") {
        format!("{app} no encontró esa playlist")
    } else {
        let line = stderr.lines().next().unwrap_or("error desconocido").trim();
        format!("{app} no pudo hacerlo: {line}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    #[test]
    fn spotify_links_of_every_shape_become_one_canonical_uri() {
        let want = Target::Spotify("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M".to_string());
        for text in [
            "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
            "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M?si=abc",
            "https://open.spotify.com/intl-es/playlist/37i9dQZF1DXcBWIGoYBM5M",
        ] {
            assert_eq!(target_of(text).unwrap(), want, "{text}");
        }
    }

    #[test]
    fn anything_else_that_looks_like_spotify_is_refused() {
        for text in [
            "spotify:playlist:x",
            "spotify:hack:37i9dQZF1DXcBWIGoYBM5M",
            "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M:extra",
            "https://open.spotify.com/",
            "",
        ] {
            assert!(target_of(text).is_err(), "{text}");
        }
    }

    #[test]
    fn a_name_cannot_break_out_of_the_script() {
        let Target::Playlist(name) = target_of(r#"Chill" & (do shell script "rm -rf ~") & ""#).unwrap() else {
            panic!()
        };
        let script = script_for(&MediaCommand::Play(Some(name.clone())), Player::Music, Some(&Target::Playlist(name)));
        // Every quote of the name is escaped: what is left outside the literal is only the fixed part.
        assert!(
            script.starts_with(
                r#"tell application "Music" to play playlist "Chill\" & (do shell script \"rm -rf ~\") & \"""#
            ),
            "{script}"
        );
        assert!(target_of("línea\nuno").is_err(), "control characters never reach a script");
        assert!(target_of(&"x".repeat(200)).is_err());
    }

    #[test]
    fn each_command_is_the_script_the_apps_understand() {
        let uri = Target::Spotify("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M".to_string());
        assert_eq!(
            script_for(&MediaCommand::Play(None), Player::Spotify, None),
            r#"tell application "Spotify" to play"#
        );
        assert_eq!(
            script_for(&MediaCommand::Play(Some(String::new())), Player::Spotify, Some(&uri)),
            r#"tell application "Spotify" to play track "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M""#
        );
        assert_eq!(script_for(&MediaCommand::Pause, Player::Music, None), r#"tell application "Music" to pause"#);
        assert_eq!(
            script_for(&MediaCommand::Next, Player::Spotify, None),
            r#"tell application "Spotify" to next track"#
        );
        assert_eq!(
            script_for(&MediaCommand::Previous, Player::Music, None),
            r#"tell application "Music" to previous track"#
        );
    }

    #[test]
    fn the_permission_problem_is_said_in_plain_words() {
        assert!(explain("execution error: Not authorized to send Apple events to Spotify. (-1743)", "Spotify")
            .contains("Automatización"));
        assert!(explain("Can't get playlist \"x\". (-1728)", "Music").contains("no encontró esa playlist"));
    }
}
