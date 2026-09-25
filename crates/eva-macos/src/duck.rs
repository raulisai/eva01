//! Quiet the Mac while the user dictates: pause the music that is playing and
//! silence the rest, then put it all back when the key comes up.
//!
//! Spotify and Apple Music are paused (and resumed) through AppleScript, but
//! only when they are open *and playing* — never launched, never started. For
//! everything else (a video in the browser, a call) there is no way to pause
//! from outside, so the system's output volume goes to zero and returns to
//! what it was, unless the user moved it meanwhile.
//!
//! What was changed is written to a small state file first, so a crash in
//! the middle of a recording does not leave the Mac muted: the next start
//! calls [`recover`].
//!
//! [`MediaDucker`] is the part that must never get wrong, and it is pure
//! logic over a [`Backend`]: a press that is released before the ducking has
//! finished still ends with everything restored.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What was changed, and so what has to be put back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ducked {
    /// The players that were playing and were paused ("Spotify", "Music").
    pub paused: Vec<String>,
    /// The output volume before it was silenced, if it was.
    pub volume: Option<u8>,
}

impl Ducked {
    /// Whether nothing was changed.
    pub fn is_empty(&self) -> bool {
        self.paused.is_empty() && self.volume.is_none()
    }

    fn to_text(&self) -> String {
        let mut lines: Vec<String> = self.paused.iter().map(|p| format!("pause:{p}")).collect();
        if let Some(volume) = self.volume {
            lines.push(format!("volume:{volume}"));
        }
        lines.join("\n")
    }

    fn from_text(text: &str) -> Ducked {
        let mut ducked = Ducked::default();
        for line in text.lines() {
            match line.split_once(':') {
                Some(("pause", app)) if PLAYERS.contains(&app) => ducked.paused.push(app.to_string()),
                Some(("volume", v)) => ducked.volume = v.trim().parse().ok().filter(|v| *v <= 100),
                _ => {}
            }
        }
        ducked
    }
}

/// The two players that can be paused from outside.
const PLAYERS: [&str; 2] = ["Spotify", "Music"];

/// What quieting and un-quieting actually does. A trait so the ordering
/// logic can be tested without touching the real volume.
pub trait Backend: Send + Sync {
    /// Quiets the Mac, returning what to put back (`None`: nothing changed).
    fn duck(&self) -> Option<Ducked>;
    /// Puts back what [`Backend::duck`] changed.
    fn restore(&self, ducked: Ducked);
}

/// Keeps the Mac quiet exactly while the user wants it quiet.
///
/// Two calls, deliberately apart: [`MediaDucker::want_quiet`] only flips a
/// flag and is instant (call it from the command loop, in the order things
/// happen), and [`MediaDucker::settle`] brings the real state in line with
/// that flag, taking as long as osascript takes (call it from a blocking
/// thread). `settle` may run late, twice or out of order with another one:
/// it looks at the flag, not at who called it, so a release that overtakes
/// its press, or comes in the middle of it, still ends with everything back.
pub struct MediaDucker {
    backend: Box<dyn Backend>,
    /// Whether the user wants it quiet right now (the key is down).
    wanted: AtomicBool,
    /// What is currently changed; held for the whole of a `settle`, so two
    /// never overlap.
    state: Mutex<Option<Ducked>>,
}

impl MediaDucker {
    /// A ducker over `backend`.
    pub fn new(backend: Box<dyn Backend>) -> MediaDucker {
        MediaDucker { backend, wanted: AtomicBool::new(false), state: Mutex::new(None) }
    }

    /// The real one: AppleScript for the players, the system volume for the
    /// rest, and `state_file` to survive a crash.
    pub fn for_this_mac(state_file: PathBuf) -> MediaDucker {
        MediaDucker::new(Box::new(MacBackend { state_file }))
    }

    /// Says whether the Mac should be quiet. Instant; changes nothing yet.
    pub fn want_quiet(&self, quiet: bool) {
        self.wanted.store(quiet, Ordering::SeqCst);
    }

    /// Makes the Mac as quiet as [`MediaDucker::want_quiet`] last said.
    pub fn settle(&self) {
        #[allow(clippy::unwrap_used)] // only poisoned if a holder panicked, forbidden by workspace policy
        let mut state = self.state.lock().unwrap();
        // Looped: the wish can change while a step is under way.
        loop {
            match (self.wanted.load(Ordering::SeqCst), state.is_some()) {
                (true, false) => match self.backend.duck().filter(|d| !d.is_empty()) {
                    Some(ducked) => *state = Some(ducked),
                    None => return, // nothing to quiet
                },
                (false, true) => {
                    if let Some(ducked) = state.take() {
                        self.backend.restore(ducked);
                    }
                }
                _ => return,
            }
        }
    }
}

/// Puts back what a run that never finished (a crash, a force-quit) left
/// changed. Call once when the app starts.
pub fn recover(state_file: &Path) {
    if let Ok(text) = std::fs::read_to_string(state_file) {
        tracing::warn!("había música pausada o el volumen en cero de una sesión anterior: se restaura");
        restore_now(&Ducked::from_text(&text));
        let _ = std::fs::remove_file(state_file);
    }
}

struct MacBackend {
    state_file: PathBuf,
}

impl Backend for MacBackend {
    fn duck(&self) -> Option<Ducked> {
        // Each step is its own `osascript` (a refusal from one app must not
        // stop the others) and they run side by side.
        let running: Vec<&str> = PLAYERS.iter().copied().filter(|p| crate::workspace::is_app_running(p)).collect();
        let (paused, volume) = std::thread::scope(|scope| {
            let players: Vec<_> = running.iter().map(|&app| scope.spawn(move || pause_if_playing(app))).collect();
            let volume = scope.spawn(silence_output);
            let paused: Vec<String> =
                players.into_iter().filter_map(|h| h.join().ok().flatten()).map(str::to_string).collect();
            (paused, volume.join().ok().flatten())
        });
        let ducked = Ducked { paused, volume };
        if !ducked.is_empty() {
            // Written before anything can go wrong afterwards.
            if let Err(e) = std::fs::write(&self.state_file, ducked.to_text()) {
                tracing::warn!("no se pudo guardar el estado del volumen: {e}");
            }
            tracing::info!(paused = ?ducked.paused, volume = ?ducked.volume, "sonido bajado mientras dictas");
        }
        Some(ducked)
    }

    fn restore(&self, ducked: Ducked) {
        restore_now(&ducked);
        let _ = std::fs::remove_file(&self.state_file);
        tracing::info!("sonido restaurado");
    }
}

fn restore_now(ducked: &Ducked) {
    // The volume first, so a resumed song does not start at zero.
    if let Some(volume) = ducked.volume {
        restore_output(volume);
    }
    for app in &ducked.paused {
        if crate::workspace::is_app_running(app) {
            let _ = osascript(&format!("tell application \"{app}\" to play"));
        }
    }
}

/// Pauses `app` if it is playing. `Some(app)` if it did.
fn pause_if_playing(app: &'static str) -> Option<&'static str> {
    let state = osascript(&format!("tell application \"{app}\" to (player state is playing)")).ok()?;
    if state.trim() != "true" {
        return None;
    }
    osascript(&format!("tell application \"{app}\" to pause")).ok().map(|_| app)
}

/// The output volume and whether the output is muted, from `get volume settings`.
fn output_volume() -> Option<(u8, bool)> {
    parse_volume(&osascript("get volume settings").ok()?)
}

fn parse_volume(settings: &str) -> Option<(u8, bool)> {
    let field =
        |name: &str| settings.split(',').find_map(|part| part.trim().strip_prefix(name).map(|v| v.trim().to_string()));
    let volume = field("output volume:")?.parse::<u8>().ok().filter(|v| *v <= 100)?;
    let muted = field("output muted:").is_some_and(|m| m == "true");
    Some((volume, muted))
}

/// Sets the output volume to zero. `Some(previous)` if there was something to
/// silence; a device with no volume control (some HDMI, some Bluetooth) or one
/// that is already silent is left alone.
fn silence_output() -> Option<u8> {
    let (volume, muted) = output_volume()?;
    if volume == 0 || muted {
        return None;
    }
    osascript("set volume output volume 0").ok()?;
    Some(volume)
}

/// Puts the volume back — unless the user moved it since (it is not zero).
fn restore_output(volume: u8) {
    if output_volume().is_some_and(|(now, _)| now == 0) {
        let _ = osascript(&format!("set volume output volume {volume}"));
    }
}

/// Runs one line of AppleScript, giving up after a few seconds.
fn osascript(script: &str) -> Result<String, String> {
    let mut child = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = std::io::Read::read_to_string(&mut stdout, &mut out);
                }
                return if status.success() { Ok(out) } else { Err("osascript falló".to_string()) };
            }
            Ok(None) if started.elapsed() > Duration::from_secs(4) => {
                let _ = child.kill();
                return Err("osascript no respondió".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Default)]
    struct Fake {
        log: Mutex<Vec<String>>,
        /// Called in the middle of `duck`, to make the key come up then.
        during_duck: Mutex<Option<Box<dyn Fn() + Send>>>,
    }

    struct FakeBackend(Arc<Fake>);

    impl Backend for FakeBackend {
        fn duck(&self) -> Option<Ducked> {
            self.0.log.lock().unwrap().push("duck".into());
            if let Some(hook) = self.0.during_duck.lock().unwrap().as_ref() {
                hook();
            }
            Some(Ducked { paused: vec!["Spotify".into()], volume: Some(40) })
        }

        fn restore(&self, ducked: Ducked) {
            self.0.log.lock().unwrap().push(format!("restore {ducked:?}"));
        }
    }

    fn ducker() -> (MediaDucker, Arc<Fake>) {
        let fake = Arc::new(Fake::default());
        (MediaDucker::new(Box::new(FakeBackend(Arc::clone(&fake)))), fake)
    }

    fn log(fake: &Fake) -> Vec<String> {
        fake.log.lock().unwrap().clone()
    }

    #[test]
    fn what_was_quieted_is_put_back_once() {
        let (ducker, fake) = ducker();
        ducker.want_quiet(true);
        ducker.settle();
        ducker.settle(); // settling again changes nothing more
        ducker.want_quiet(false);
        ducker.settle();
        ducker.settle();
        assert_eq!(log(&fake), vec!["duck", "restore Ducked { paused: [\"Spotify\"], volume: Some(40) }"]);
    }

    #[test]
    fn a_tap_released_before_anything_was_done_quiets_nothing() {
        let (ducker, fake) = ducker();
        ducker.want_quiet(true);
        ducker.want_quiet(false);
        // Both jobs run, in either order: neither finds anything to do.
        ducker.settle();
        ducker.settle();
        assert!(log(&fake).is_empty());
    }

    #[test]
    fn a_key_released_in_the_middle_of_quieting_ends_with_everything_restored() {
        let (ducker, fake) = ducker();
        let ducker = Arc::new(ducker);
        let releasing = Arc::clone(&ducker);
        *fake.during_duck.lock().unwrap() = Some(Box::new(move || releasing.want_quiet(false)));
        ducker.want_quiet(true);
        ducker.settle();
        let entries = log(&fake);
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(entries[0], "duck");
        assert!(entries[1].starts_with("restore"), "{entries:?}");
    }

    #[test]
    fn a_second_press_after_a_release_quiets_again() {
        let (ducker, fake) = ducker();
        for _ in 0..2 {
            ducker.want_quiet(true);
            ducker.settle();
            ducker.want_quiet(false);
            ducker.settle();
        }
        assert_eq!(log(&fake).iter().filter(|l| *l == "duck").count(), 2);
        assert_eq!(log(&fake).iter().filter(|l| l.starts_with("restore")).count(), 2);
    }

    #[test]
    fn the_state_file_round_trips_and_ignores_what_it_does_not_know() {
        let ducked = Ducked { paused: vec!["Spotify".into(), "Music".into()], volume: Some(35) };
        assert_eq!(Ducked::from_text(&ducked.to_text()), ducked);
        let odd = Ducked::from_text("pause:Rm -rf\nvolume:900\nbasura\npause:Music");
        assert_eq!(odd, Ducked { paused: vec!["Music".into()], volume: None });
    }

    #[test]
    fn the_volume_settings_line_is_read() {
        assert_eq!(
            parse_volume("output volume:47, input volume:70, alert volume:100, output muted:false"),
            Some((47, false))
        );
        assert_eq!(
            parse_volume("output volume:0, input volume:70, alert volume:100, output muted:true"),
            Some((0, true))
        );
        assert_eq!(parse_volume("output volume:missing value, input volume:70"), None, "no volume control");
    }
}
