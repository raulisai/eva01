//! Harvesting the eval corpus from ordinary use (`docs/PLAN.md` fase 3, point
//! 6): when a dictation comes out wrong the user flags it, and the audio and
//! text that produced it are kept as a case to measure the next change on.
//!
//! What is remembered before the flag is only the *last* dictation, and only
//! in memory: nothing about what was said is written to disk unless the user
//! says it went wrong. A dictation into a password field is not even
//! remembered.

use eva_store::{Store, StoreError};
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use thiserror::Error;
use uuid::Uuid;

/// The last dictation's text, and where it was saved if it was.
#[derive(Debug, Clone)]
struct Dictation {
    request_id: Uuid,
    transcript_id: Option<Uuid>,
    raw: String,
    /// What was pasted; `None` when the utterance was a command.
    formatted: Option<String>,
}

#[derive(Default)]
struct Remembered {
    audio: Option<(Uuid, Vec<f32>)>,
    dictation: Option<Dictation>,
}

/// What the last flag kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flagged {
    /// The name (without extension) of the files written in [`Harvest::dir`].
    pub stem: String,
    /// Whether the audio was still there to keep (typed text has none).
    pub audio_saved: bool,
}

/// Why a dictation could not be flagged.
#[derive(Debug, Error)]
pub enum FlagError {
    /// Nothing has been dictated since the worker started (or the last one
    /// went into a password field).
    #[error("no hay ningún dictado reciente que marcar")]
    NothingToFlag,
    /// The database refused the flag.
    #[error("no se pudo marcar el dictado: {0}")]
    Store(#[from] StoreError),
    /// The folder or the text file could not be written.
    #[error("no se pudo guardar el dictado en {dir}: {source}")]
    Io {
        /// The harvest folder.
        dir: String,
        /// What failed.
        source: io::Error,
    },
    /// The audio file could not be written.
    #[error(transparent)]
    Wav(#[from] eva_audio::wav::WavError),
}

/// The last dictation, held for [`Harvest::flag_last`].
pub struct Harvest {
    dir: PathBuf,
    remembered: Mutex<Remembered>,
}

impl Harvest {
    /// A harvest that writes flagged dictations to `dir` (created on demand).
    pub fn new(dir: PathBuf) -> Harvest {
        Harvest { dir, remembered: Mutex::new(Remembered::default()) }
    }

    /// Where flagged dictations are kept.
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Holds the audio of the recording `request_id`, replacing the last one.
    pub fn remember_audio(&self, request_id: Uuid, samples: Vec<f32>) {
        self.state().audio = Some((request_id, samples));
    }

    /// Holds the text the recording (or typed request) `request_id` produced.
    /// `transcript_id` is its row in the database, when it was saved there;
    /// `formatted` is what was pasted, or `None` for a command.
    pub fn remember_dictation(
        &self,
        request_id: Uuid,
        transcript_id: Option<Uuid>,
        raw: &str,
        formatted: Option<&str>,
    ) {
        self.state().dictation = Some(Dictation {
            request_id,
            transcript_id,
            raw: raw.to_string(),
            formatted: formatted.map(str::to_string),
        });
    }

    /// Drops everything held — what happens to a dictation that must leave no
    /// trace (a password field).
    pub fn forget(&self) {
        *self.state() = Remembered::default();
    }

    /// Flags the last utterance as wrong: marks it in the database, and
    /// writes its audio (`<stem>.wav`) and what was heard and pasted
    /// (`<stem>.propuesta.txt`) to [`Harvest::dir`].
    ///
    /// The text file is a *proposal*, not the reference: the reference is what
    /// the user actually said, which only they know. Writing it as
    /// `<stem>.txt` is what turns the pair into a corpus sample.
    ///
    /// # Errors
    /// [`FlagError`] if there is nothing to flag or something could not be written.
    pub fn flag_last(&self, store: &Store) -> Result<Flagged, FlagError> {
        let (dictation, audio) = {
            let held = self.state();
            let dictation = held.dictation.clone().ok_or(FlagError::NothingToFlag)?;
            let audio = held.audio.as_ref().filter(|(id, _)| *id == dictation.request_id).map(|(_, s)| s.clone());
            (dictation, audio)
        };

        if let Some(id) = dictation.transcript_id {
            store.mark_transcript_bad(id)?;
        }

        let stem = dictation.transcript_id.unwrap_or(dictation.request_id).to_string();
        let io_error = |source| FlagError::Io { dir: self.dir.display().to_string(), source };
        std::fs::create_dir_all(&self.dir).map_err(io_error)?;

        let audio_saved = match audio {
            Some(samples) => {
                eva_audio::wav::write_mono_16k(&self.dir.join(format!("{stem}.wav")), &samples)?;
                true
            }
            None => false,
        };
        let pasted = dictation.formatted.as_deref().unwrap_or("(era una orden; no se pegó nada)");
        let proposal = format!("oído:   {}\npegado: {pasted}\n", dictation.raw);
        std::fs::write(self.dir.join(format!("{stem}.propuesta.txt")), proposal).map_err(io_error)?;

        Ok(Flagged { stem, audio_saved })
    }

    fn state(&self) -> MutexGuard<'_, Remembered> {
        #[allow(clippy::unwrap_used)] // only poisoned if a prior lock-holder panicked, forbidden by workspace policy
        self.remembered.lock().unwrap()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn harvest() -> (Harvest, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Harvest::new(dir.path().join("harvest")), dir)
    }

    #[test]
    fn with_nothing_dictated_there_is_nothing_to_flag() {
        let (harvest, _dir) = harvest();
        let error = harvest.flag_last(&Store::open_in_memory().unwrap()).unwrap_err();
        assert!(matches!(error, FlagError::NothingToFlag));
    }

    #[test]
    fn flagging_marks_the_row_and_keeps_the_audio_and_a_text_proposal() {
        let (harvest, _dir) = harvest();
        let store = Store::open_in_memory().unwrap();
        let (request, transcript) = (Uuid::new_v4(), store.save_transcript("hay que", "hay que", "Hay que.").unwrap());
        harvest.remember_audio(request, vec![0.2; 1_600]);
        harvest.remember_dictation(request, Some(transcript), "hay que", Some("Hay que."));

        let flagged = harvest.flag_last(&store).unwrap();

        assert_eq!(flagged, Flagged { stem: transcript.to_string(), audio_saved: true });
        assert!(store.transcripts_marked_bad().unwrap().iter().any(|t| t.id == transcript));
        assert!(harvest.dir().join(format!("{transcript}.wav")).is_file());
        let proposal = std::fs::read_to_string(harvest.dir().join(format!("{transcript}.propuesta.txt"))).unwrap();
        assert_eq!(proposal, "oído:   hay que\npegado: Hay que.\n");
        assert!(
            !harvest.dir().join(format!("{transcript}.txt")).exists(),
            "the reference is what was really said — only the user can write it"
        );
    }

    #[test]
    fn audio_from_another_recording_is_never_attached() {
        let (harvest, _dir) = harvest();
        let store = Store::open_in_memory().unwrap();
        harvest.remember_audio(Uuid::new_v4(), vec![0.2; 1_600]);
        let typed = Uuid::new_v4();
        harvest.remember_dictation(typed, None, "hola", Some("Hola."));

        let flagged = harvest.flag_last(&store).unwrap();

        assert!(!flagged.audio_saved);
        assert_eq!(flagged.stem, typed.to_string(), "without a database row the request id names the files");
        assert!(!harvest.dir().join(format!("{typed}.wav")).exists());
    }

    #[test]
    fn a_forgotten_dictation_cannot_be_flagged() {
        let (harvest, _dir) = harvest();
        let request = Uuid::new_v4();
        harvest.remember_audio(request, vec![0.2; 1_600]);
        harvest.remember_dictation(request, None, "mi contraseña es", Some("Mi contraseña es."));

        harvest.forget();

        assert!(matches!(harvest.flag_last(&Store::open_in_memory().unwrap()), Err(FlagError::NothingToFlag)));
        assert!(!harvest.dir().exists(), "nothing is written unless something is flagged");
    }

    #[test]
    fn flagging_twice_is_harmless() {
        let (harvest, _dir) = harvest();
        let store = Store::open_in_memory().unwrap();
        let request = Uuid::new_v4();
        harvest.remember_dictation(request, None, "a", Some("A."));
        assert_eq!(harvest.flag_last(&store).unwrap(), harvest.flag_last(&store).unwrap());
    }

    #[test]
    fn a_misheard_command_can_be_flagged_and_its_proposal_says_it_was_a_command() {
        let (harvest, _dir) = harvest();
        let request = Uuid::new_v4();
        harvest.remember_audio(request, vec![0.2; 1_600]);
        harvest.remember_dictation(request, None, "adán abre grave", None);

        let flagged = harvest.flag_last(&Store::open_in_memory().unwrap()).unwrap();

        assert!(flagged.audio_saved);
        let proposal = std::fs::read_to_string(harvest.dir().join(format!("{}.propuesta.txt", flagged.stem))).unwrap();
        assert!(proposal.starts_with("oído:   adán abre grave\n"));
        assert!(proposal.contains("era una orden"));
    }
}
