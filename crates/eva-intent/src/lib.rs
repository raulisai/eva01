#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(missing_docs)]

//! The intention layer: decides whether a transcript is plain dictation or a
//! command, and if it's a command, what it means. See `docs/PLAN.md` §3.2
//! (why there is no level-1 LLM router) and fase 4 (this crate's scope: it
//! classifies, it never executes).

pub mod apps;
pub mod calibration;
pub mod catalog;
pub mod context;
pub mod intent;
pub mod risk;
pub mod sites;
pub mod sound;
pub mod spoken;
pub mod steps;
pub mod wake;

pub use apps::{AppEntry, AppIndex};
pub use intent::Intent;
pub use risk::Risk;
pub use steps::Step;

/// The result of running the whole intention layer on a raw transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpretResult {
    /// No wake word matched: this is plain dictation, to be cleaned by
    /// `eva-text` and pasted, not touched by this crate any further.
    Dictation,
    /// The wake word matched and the remainder parsed into `Intent`.
    Command(Intent),
}

/// Runs the full intention pipeline on a raw transcript: strips the wake
/// word, and if present, parses the remainder into an [`Intent`]. This is
/// what both `eva-worker`'s production dictation path and the `eva intent`
/// CLI command call — see `docs/PLAN.md` §3.1, "esto es lo que hace testeable
/// la capa de intención sin grabar audio."
pub fn interpret(raw_text: &str, wake_word: &str, app_index: &AppIndex) -> InterpretResult {
    interpret_with(raw_text, wake_word, app_index, &[])
}

/// Like [`interpret`], with the user's own phrases (`[[commands]]`): if what
/// follows the wake word is exactly one of them, it is [`Intent::Custom`] —
/// checked before any built-in rule, since the user wrote it on purpose (the
/// gateway still rules on whatever it does).
pub fn interpret_with(raw_text: &str, wake_word: &str, app_index: &AppIndex, custom: &[&str]) -> InterpretResult {
    interpret_tolerant(raw_text, wake_word, app_index, custom, &[]).result
}

/// What [`interpret_tolerant`] made of a transcript, and how the wake word
/// was heard (when there was one), so the caller can learn from it.
#[derive(Debug, Clone, PartialEq)]
pub struct Interpreted {
    /// Dictation or a command.
    pub result: InterpretResult,
    /// How the wake word was heard and the word as heard, if it was taken as one.
    pub wake: Option<(wake::WakeMatch, String)>,
}

/// Like [`interpret_with`], with a wake word that speech recognition does not
/// always get right: a spelling in `learned` (confirmed before for this
/// user) is the wake word; one merely *close* to it ("Adam" for "Adán") is,
/// but only when what follows is clearly a command — an open, close, search,
/// one of the user's phrases — never a free-form task, which is too costly
/// to start from a guess.
pub fn interpret_tolerant(
    raw_text: &str,
    wake_word: &str,
    app_index: &AppIndex,
    custom: &[&str],
    learned: &[String],
) -> Interpreted {
    let dictation = Interpreted { result: InterpretResult::Dictation, wake: None };
    let Some(found) = wake::find_wake_word(raw_text, wake_word, learned) else { return dictation };
    let intent = match custom.iter().find(|phrase| intent::is_phrase(found.rest, phrase)) {
        Some(phrase) => Intent::Custom { phrase: (*phrase).to_string() },
        None => {
            let parsed = intent::parse(found.rest, app_index);
            // Nothing matched, so this would go to an agent as a task: if it is
            // nearly one of the user's phrases, ask instead. Not on a wake word
            // that was only guessed at — a question out of nowhere is noise.
            let would_be_a_task = matches!(parsed, Intent::AgentTask { provider: None, .. });
            match intent::nearest_phrase(found.rest, custom)
                .filter(|_| would_be_a_task && found.how != wake::WakeMatch::Similar)
            {
                Some(phrase) => Intent::ConfirmCustom {
                    heard: found.rest.trim().trim_end_matches(['.', '!', '?']).to_string(),
                    phrase: phrase.to_string(),
                },
                None => parsed,
            }
        }
    };
    if found.how == wake::WakeMatch::Similar && !intent.is_clear_command() {
        return dictation;
    }
    Interpreted { result: InterpretResult::Command(intent), wake: Some((found.how, found.heard)) }
}

/// Like [`interpret_tolerant`], leaning on what was just done (`recall`, from
/// [`context::Recall::of`]) while the conversation is open: a search after
/// "abre YouTube" is YouTube's own, and a plain "ahora busca Naruto" — said
/// with no wake word — is a command instead of dictation. Whatever it does
/// not change comes out exactly as [`interpret_tolerant`] gives it.
pub fn interpret_in_context(
    raw_text: &str,
    wake_word: &str,
    app_index: &AppIndex,
    custom: &[&str],
    learned: &[String],
    recall: Option<&context::Recall>,
) -> Interpreted {
    let mut interpreted = interpret_tolerant(raw_text, wake_word, app_index, custom, learned);
    let Some(recall) = recall else { return interpreted };
    match &interpreted.result {
        InterpretResult::Command(Intent::WebSearch { query }) => {
            if let Some(intent) = recall.search(query) {
                interpreted.result = InterpretResult::Command(intent);
            }
        }
        InterpretResult::Dictation => {
            if let Some(intent) = context::follow_up(raw_text, recall) {
                interpreted = Interpreted { result: InterpretResult::Command(intent), wake: None };
            }
        }
        InterpretResult::Command(_) => {}
    }
    interpreted
}

/// `interpreted` with an app that is not installed but is a website EVA01
/// knows ("abre YouTube") turned into opening the site. The last step, after
/// the apps have been looked at again.
pub fn with_known_sites(mut interpreted: Interpreted) -> Interpreted {
    if let InterpretResult::Command(intent) = interpreted.result {
        interpreted.result = InterpretResult::Command(sites::open_by_name(intent));
    }
    interpreted
}

/// Whether `raw_text` is, beyond doubt, a spoken command: the wake word (as
/// heard, however close) followed by something that is not a task for an
/// agent. What the two speech models' answers are compared by: the one that
/// is a command wins. It knows no apps — an unknown name still counts.
pub fn looks_like_command(raw_text: &str, wake_word: &str, learned: &[String]) -> bool {
    let no_apps = AppIndex::new(Vec::new());
    matches!(
        interpret_tolerant(raw_text, wake_word, &no_apps, &[], learned).result,
        InterpretResult::Command(intent) if intent.is_clear_command()
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::apps::AppEntry;

    fn sample_index() -> AppIndex {
        AppIndex::new(vec![AppEntry::new("Brave Browser").with_aliases(["brave"])])
    }

    #[test]
    fn text_without_the_wake_word_is_plain_dictation() {
        let result = interpret("mañana voy a abrir Brave", "Adán", &sample_index());
        assert_eq!(result, InterpretResult::Dictation);
    }

    #[test]
    fn text_with_the_wake_word_becomes_a_command() {
        let result = interpret("Adán, abre Brave", "Adán", &sample_index());
        assert_eq!(result, InterpretResult::Command(Intent::OpenApp { app: "Brave Browser".to_string() }));
    }

    #[test]
    fn the_end_to_end_headline_fix_the_wake_word_works_without_its_accent() {
        // This is the scenario from docs/PLAN.md §2A Hallazgo 4: the STT
        // engine renders the wake word without its tilde. It must still
        // gate as a command, not get pasted as dictation.
        let result = interpret("adan abre brave", "Adán", &sample_index());
        assert_eq!(result, InterpretResult::Command(Intent::OpenApp { app: "Brave Browser".to_string() }));
    }

    #[test]
    fn a_users_own_phrase_wins_over_the_built_in_rules_and_ignores_accents_and_punctuation() {
        // "abre brave" would be OpenApp; the user's phrase gets there first.
        let custom = ["abre brave", "mi correo"];
        for spoken in ["Adán, mi correo.", "adan, Mí  correo", "Adán: MI CORREO!"] {
            assert_eq!(
                interpret_with(spoken, "Adán", &sample_index(), &custom),
                InterpretResult::Command(Intent::Custom { phrase: "mi correo".to_string() }),
                "{spoken}"
            );
        }
        assert_eq!(
            interpret_with("Adán, abre Brave", "Adán", &sample_index(), &custom),
            InterpretResult::Command(Intent::Custom { phrase: "abre brave".to_string() })
        );
    }

    #[test]
    fn a_phrase_that_is_nearly_the_users_gets_a_question_not_an_agent() {
        let custom = ["ver mi canal favorito"];
        let asked = |spoken: &str| interpret_with(spoken, "Adán", &sample_index(), &custom);
        // Most of the words are the same, or the letters barely differ.
        for said in ["Adán, ponme mi canal favorito.", "Adán, ver mi canal favorite", "adan ver mi canal favoritos"] {
            assert!(
                matches!(asked(said), InterpretResult::Command(Intent::ConfirmCustom { ref phrase, .. }) if phrase == "ver mi canal favorito"),
                "{said}: {:?}",
                asked(said)
            );
        }
        // Talking about the same subject is not asking for the command.
        for said in ["Adán, mi canal favorito tiene un problema con las miniaturas", "Adán, agrega tests al login"] {
            assert!(matches!(asked(said), InterpretResult::Command(Intent::AgentTask { .. })), "{said}");
        }
    }

    #[test]
    fn a_near_phrase_never_beats_a_rule_and_never_follows_a_guessed_wake_word() {
        let custom = ["abre spotify ya"];
        // "abre spotify" is an app command whatever the user's phrases are.
        assert!(matches!(
            interpret_with("Adán, abre Brave", "Adán", &sample_index(), &custom),
            InterpretResult::Command(Intent::OpenApp { .. })
        ));
        let custom = ["ver mi canal favorito"];
        assert_eq!(
            interpret_tolerant("Adam ponme mi canal favorito", "Adán", &sample_index(), &custom, &[]).result,
            InterpretResult::Dictation
        );
    }

    #[test]
    fn a_phrase_must_match_the_whole_command_not_a_part_of_it() {
        let custom = ["mi correo"];
        assert!(matches!(
            interpret_with("Adán, mi correo es un desastre", "Adán", &sample_index(), &custom),
            InterpretResult::Command(Intent::AgentTask { .. })
        ));
        assert_eq!(interpret_with("mi correo", "Adán", &sample_index(), &custom), InterpretResult::Dictation);
    }

    #[test]
    fn a_command_that_would_be_destructive_is_blocked_end_to_end() {
        let result = interpret("Adán, borra el proyecto", "Adán", &sample_index());
        assert!(matches!(result, InterpretResult::Command(Intent::Blocked { .. })));
    }

    #[test]
    fn a_wake_word_heard_as_adam_runs_a_clear_command_but_never_starts_a_task() {
        let index = AppIndex::new(vec![AppEntry::new("Spotify")]);
        let heard = |text: &str| interpret_tolerant(text, "Adán", &index, &[], &[]);

        let open = heard("Adam, abre Spotify.");
        assert_eq!(open.result, InterpretResult::Command(Intent::OpenApp { app: "Spotify".to_string() }));
        assert_eq!(open.wake, Some((wake::WakeMatch::Similar, "adam".to_string())));

        // A free-form task after a look-alike word is just someone talking.
        assert_eq!(heard("Adam agrega tests al login").result, InterpretResult::Dictation);
        assert_eq!(heard("Adam es mi amigo").result, InterpretResult::Dictation);
    }

    #[test]
    fn a_confirmed_spelling_is_trusted_for_anything() {
        let index = AppIndex::new(Vec::new());
        let learned = vec!["adam".to_string()];
        let task = interpret_tolerant("Adam agrega tests al login", "Adán", &index, &[], &learned);
        assert!(matches!(task.result, InterpretResult::Command(Intent::AgentTask { .. })));
        assert_eq!(task.wake.map(|(how, _)| how), Some(wake::WakeMatch::Learned));
    }

    #[test]
    fn of_two_transcripts_the_one_that_is_a_command_is_the_one_to_keep() {
        // What the speech models really produced for "Adán, abre Spotify".
        assert!(!looks_like_command("Adán, Avrey es Potty Pye.", "Adán", &[]));
        assert!(looks_like_command("Adam abre Spotify.", "Adán", &[]));
        assert!(looks_like_command("Adán, abre Brave.", "Adán", &[]));
        assert!(!looks_like_command("Hola, me puedes abrir Spotify.", "Adán", &[]));
    }

    #[test]
    fn a_website_is_opened_by_name_when_no_app_has_it_and_the_app_wins_when_there_is_one() {
        let index = AppIndex::new(vec![AppEntry::new("Spotify")]);
        let heard = |text: &str| with_known_sites(interpret_tolerant(text, "Adán", &index, &[], &[])).result;
        assert_eq!(
            heard("Adán, abre YouTube"),
            InterpretResult::Command(Intent::OpenUrl { url: "https://www.youtube.com".to_string() })
        );
        assert_eq!(
            heard("Adán, abre Spotify"),
            InterpretResult::Command(Intent::OpenApp { app: "Spotify".to_string() }),
            "an installed app is preferred to its website"
        );
        assert!(matches!(heard("Adán, abre Photoshop"), InterpretResult::Command(Intent::AppNotFound { .. })));
    }

    #[test]
    fn the_youtube_conversation_end_to_end() {
        let index = AppIndex::new(Vec::new());
        let opened = match with_known_sites(interpret_tolerant("Adán, abre YouTube", "Adán", &index, &[], &[])).result
        {
            InterpretResult::Command(intent) => intent,
            other => panic!("{other:?}"),
        };
        let recall = context::Recall::of(&opened);
        let naruto = Some(InterpretResult::Command(Intent::OpenUrl {
            url: "https://www.youtube.com/results?search_query=Naruto".to_string(),
        }));

        // With the wake word or without it, in the seconds after.
        for said in ["Adán, busca Naruto", "ahora busca Naruto", "Busca Naruto."] {
            let got = interpret_in_context(said, "Adán", &index, &[], &[], recall.as_ref());
            assert_eq!(Some(got.result), naruto, "{said}");
        }
        // Outside the conversation the same words are a web search / plain dictation.
        assert!(matches!(
            interpret_in_context("Adán, busca Naruto", "Adán", &index, &[], &[], None).result,
            InterpretResult::Command(Intent::WebSearch { .. })
        ));
        assert_eq!(
            interpret_in_context("ahora busca Naruto", "Adán", &index, &[], &[], None).result,
            InterpretResult::Dictation
        );
        // And ordinary dictation inside the conversation stays dictation.
        assert_eq!(
            interpret_in_context("mañana tengo que ir al banco", "Adán", &index, &[], &[], recall.as_ref()).result,
            InterpretResult::Dictation
        );
    }
}
