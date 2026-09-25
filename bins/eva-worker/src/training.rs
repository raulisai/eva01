//! Training mode: keeping the audio of what the user said, on this Mac, so it
//! can be checked against what came out. Every number this project has about
//! accuracy came from synthetic voices; the only way to know how it hears *this*
//! person is a corpus of their own recordings with the words they really said.
//!
//! Off by default and opt-in, from the panel's «Revisar» page (or `[history]
//! save_audio`). What is kept, per utterance, is `<id>.wav` (16 kHz mono) and
//! `<id>.json` (when, what was heard, what was pasted), in `training/` next to
//! `harvest/`. Reviewing it in the panel writes `<id>.txt` — the reference, in
//! the format `eva-eval --corpus` reads. Unreviewed samples are deleted after
//! `[history] keep_days`; reviewed ones are the corpus and stay. Nothing is
//! kept from a password field.

use crate::context::WorkerContext;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// The settings key the panel's switch writes (it wins over the config file, so
/// turning it on or off needs no restart).
pub const SETTING: &str = "training_audio";

/// Where the samples are kept.
pub fn dir_of(harvest_dir: &Path) -> PathBuf {
    harvest_dir.with_file_name("training")
}

/// Whether utterances are being kept: the switch in the panel, else the config.
pub fn enabled(ctx: &WorkerContext) -> bool {
    ctx.store.get_setting::<bool>(SETTING).ok().flatten().unwrap_or(ctx.config.history.save_audio)
}

/// Keeps the audio of `request_id` with what was heard (`raw`) and what was
/// pasted (`formatted`, `None` for a command). Does nothing when training is
/// off, there is no audio for that request (typed text), or the focus is a
/// password field. A failure is logged, never shown: this must not get in the
/// way of a dictation.
pub fn save(ctx: &WorkerContext, request_id: Uuid, raw: &str, formatted: Option<&str>) {
    if !enabled(ctx) || ctx.desktop.secure_input_active() {
        return;
    }
    let Some(samples) = ctx.harvest.audio_of(request_id) else { return };
    let dir = dir_of(ctx.harvest.dir());
    if let Err(e) = write(&dir, request_id, &samples, raw, formatted) {
        tracing::warn!("no se pudo guardar el audio de entrenamiento: {e}");
    }
}

fn write(dir: &Path, id: Uuid, samples: &[f32], raw: &str, formatted: Option<&str>) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    eva_audio::wav::write_mono_16k(&dir.join(format!("{id}.wav")), samples).map_err(|e| e.to_string())?;
    let kind = match formatted {
        None => "command",
        Some(_) if raw.trim().is_empty() => "empty",
        Some(_) => "dictation",
    };
    let sidecar = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "kind": kind,
        "raw": raw,
        "formatted": formatted,
        "seconds": (samples.len() as f64 / 16_000.0 * 10.0).round() / 10.0,
    });
    std::fs::write(dir.join(format!("{id}.json")), sidecar.to_string()).map_err(|e| e.to_string())
}

/// Deletes the samples older than `keep_days` that were never reviewed (no
/// `<id>.txt`); `0` keeps everything. Returns how many were deleted.
pub fn prune(dir: &Path, keep_days: u32) -> usize {
    if keep_days == 0 {
        return 0;
    }
    let limit = Duration::from_secs(u64::from(keep_days) * 86_400);
    let mut deleted = 0;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "wav") || path.with_extension("txt").exists() {
            continue;
        }
        let age =
            entry.metadata().and_then(|m| m.modified()).ok().and_then(|m| SystemTime::now().duration_since(m).ok());
        if age.is_some_and(|age| age > limit) {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(path.with_extension("json"));
            deleted += 1;
        }
    }
    deleted
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::testkit::Rig;

    fn kept(ctx: &WorkerContext) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir_of(ctx.harvest.dir()))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path().extension().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn nothing_is_kept_unless_the_user_turned_it_on() {
        let rig = Rig::new();
        let id = Uuid::new_v4();
        rig.ctx.harvest.remember_audio(id, vec![0.2; 16_000]);
        save(&rig.ctx, id, "hola", Some("Hola."));
        assert!(kept(&rig.ctx).is_empty());
        assert!(!enabled(&rig.ctx));
    }

    #[test]
    fn with_the_switch_on_the_audio_and_what_was_heard_are_kept_and_the_setting_wins_over_the_file() {
        let rig = Rig::new();
        rig.ctx.store.set_setting(SETTING, &true).unwrap();
        let (id, other) = (Uuid::new_v4(), Uuid::new_v4());
        rig.ctx.harvest.remember_audio(id, vec![0.2; 16_000]);
        save(&rig.ctx, id, "hola", Some("Hola."));
        save(&rig.ctx, other, "sin audio", Some("Sin audio."));
        assert_eq!(kept(&rig.ctx), ["json", "wav"], "one utterance; typed text has no audio to keep");

        let sidecar: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir_of(rig.ctx.harvest.dir()).join(format!("{id}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            (sidecar["kind"].as_str(), sidecar["raw"].as_str(), sidecar["seconds"].as_f64()),
            (Some("dictation"), Some("hola"), Some(1.0))
        );
    }

    #[test]
    fn an_empty_answer_and_a_command_are_kept_as_what_they_were() {
        let rig = Rig::new();
        rig.ctx.store.set_setting(SETTING, &true).unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        rig.ctx.harvest.remember_audio(a, vec![0.2; 16_000]);
        save(&rig.ctx, a, "", Some(""));
        rig.ctx.harvest.remember_audio(b, vec![0.2; 16_000]);
        save(&rig.ctx, b, "adán abre brave", None);
        let kind = |id: Uuid| -> String {
            let text = std::fs::read_to_string(dir_of(rig.ctx.harvest.dir()).join(format!("{id}.json"))).unwrap();
            serde_json::from_str::<serde_json::Value>(&text).unwrap()["kind"].as_str().unwrap().to_string()
        };
        assert_eq!((kind(a).as_str(), kind(b).as_str()), ("empty", "command"));
    }

    #[test]
    fn only_old_unreviewed_samples_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(40 * 86_400);
        for _ in 0..3 {
            write(dir.path(), Uuid::new_v4(), &[0.2; 100], "x", Some("X")).unwrap();
        }
        // Three samples, two of them aged; the second of those was reviewed.
        let ids: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "wav"))
            .map(|e| e.path().file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        for id in &ids[..2] {
            std::fs::File::options()
                .write(true)
                .open(dir.path().join(format!("{id}.wav")))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        std::fs::write(dir.path().join(format!("{}.txt", ids[1])), "lo que dije").unwrap();

        assert_eq!(prune(dir.path(), 0), 0, "0 keeps everything");
        assert_eq!(prune(dir.path(), 30), 1, "the old one that was never reviewed");
        assert!(dir.path().join(format!("{}.wav", ids[1])).exists(), "reviewed: it is the corpus");
        assert!(dir.path().join(format!("{}.wav", ids[2])).exists(), "recent");
        assert!(!dir.path().join(format!("{}.json", ids[0])).exists());
    }
}
