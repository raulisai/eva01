//! `eva panel <route>`: the data behind the shell's panel window. Reads one
//! JSON request on stdin, prints one JSON answer (`{"ok":true,"data":…}` or
//! `{"ok":false,"error":…}`) and exits — so the shell, which is deliberately
//! small, never links the database or the config editor: it runs this and
//! relays the answer.
//!
//! Everything here is local (the database, `config.toml`, `commands/`, the
//! harvest folder). Nothing is limited or metered: what a hosted dictation
//! app sells as "Pro" — history without a cap, a dictionary, snippets, styles,
//! stats — is data this Mac already has.

use chrono::{DateTime, Local, NaiveDate, Utc};
use eva_config::{support_dir, ActionKind, CommandAction, CommandConfig, Config, Origin};
use eva_store::Store;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use toml_edit::{value, Array, ArrayOfTables, Document, Item, Table};
use uuid::Uuid;

/// Runs one route: request on stdin, answer on stdout. Always exits `0`; a
/// failure is in the answer, where the page can show it.
pub fn run(route: &str) -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let body: Value =
        if input.trim().is_empty() { json!({}) } else { serde_json::from_str(&input).unwrap_or(json!({})) };
    let answer = match dispatch(route, &body) {
        Ok(data) => json!({ "ok": true, "data": data }),
        Err(error) => json!({ "ok": false, "error": error }),
    };
    println!("{answer}");
    0
}

fn dispatch(route: &str, body: &Value) -> Result<Value, String> {
    match route {
        "overview" => overview(),
        "history" => history(body),
        "history.flag" => flag(body),
        "feedback.get" => feedback_get(body),
        "feedback.analyze" => feedback_analyze(body),
        "feedback.save" => feedback_save(body),
        "corrections" => corrections(),
        "corrections.forget" => corrections_forget(body),
        "dictionary" => dictionary(),
        "dictionary.add" => dictionary_change(body, true),
        "dictionary.remove" => dictionary_change(body, false),
        "alias.add" => alias(body, true),
        "alias.forget" => alias(body, false),
        "commands" => commands(),
        "commands.save" => commands_save(body),
        "commands.delete" => commands_delete(body),
        "commands.test" => commands_test(body),
        "commands.run" => commands_run(body),
        "commands.suggest" => commands_suggest(body),
        "commands.forget_phrase" => commands_forget_phrase(body),
        "training" => training_list(body),
        "training.set" => training_set(body),
        "training.review" => training_review(body),
        "training.approve" => training_approve(body),
        "training.discard" => training_discard(body),
        "audit" => audit(body),
        "tasks" => tasks(),
        "config" => config_get(),
        "config.set" => config_set(body),
        "wake_word.set" => wake_word_set(body),
        "calibration" => calibration(),
        "calibration.correct" => calibration_correct(body),
        "calibration.done" => calibration_done(),
        "learned" => learned(),
        "learned.forget" => learned_forget(body),
        "models" => models(),
        "resolver.test" => resolver_test(body),
        "info" => Ok(info()),
        other => Err(format!("ruta desconocida: {other}")),
    }
}

// ---- shared ----

fn db_path() -> PathBuf {
    support_dir().join("eva.sqlite3")
}

fn store() -> Result<Store, String> {
    Store::open(&db_path()).map_err(|e| format!("no se pudo abrir la base de datos: {e}"))
}

fn text_of<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

fn harvest_dir() -> PathBuf {
    support_dir().join("harvest")
}

fn uuid_of(body: &Value, key: &str) -> Result<Uuid, String> {
    Uuid::parse_str(text_of(body, key)).map_err(|_| "identificador no válido".to_string())
}

fn words_in(text: &str) -> usize {
    text.split_whitespace().count()
}

fn local_day(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&Local).date_naive()
}

fn info() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "support_dir": support_dir().display().to_string(),
        "config_path": Config::default_path().display().to_string(),
        "commands_dir": Config::commands_dir().display().to_string(),
        "harvest_dir": harvest_dir().display().to_string(),
        "database": db_path().display().to_string(),
    })
}

// ---- dictations: overview, history, flagging ----

fn overview() -> Result<Value, String> {
    let loaded = Config::load();
    let store = store()?;
    let records = store.recent_transcripts(100_000).map_err(|e| e.to_string())?;

    let total_words: usize = records.iter().map(|r| words_in(&r.formatted)).sum();
    let today = Local::now().date_naive();
    let mut per_day: BTreeMap<NaiveDate, (usize, usize)> = BTreeMap::new();
    for record in &records {
        let entry = per_day.entry(local_day(record.created_at)).or_default();
        entry.0 += words_in(&record.formatted);
        entry.1 += 1;
    }

    // Consecutive days with at least one dictation, counting back from today
    // (or from yesterday, when today has none yet: the streak is still alive).
    let mut streak = 0u32;
    let mut day = if per_day.contains_key(&today) { today } else { today.pred_opt().unwrap_or(today) };
    while per_day.contains_key(&day) {
        streak += 1;
        match day.pred_opt() {
            Some(previous) => day = previous,
            None => break,
        }
    }

    let last_days: Vec<Value> = (0..14)
        .rev()
        .filter_map(|ago| today.checked_sub_days(chrono::Days::new(ago)))
        .map(|d| {
            let (words, dictations) = per_day.get(&d).copied().unwrap_or((0, 0));
            json!({ "day": d.to_string(), "words": words, "dictations": dictations })
        })
        .collect();

    let (today_words, today_dictations) = per_day.get(&today).copied().unwrap_or((0, 0));
    let flagged_total = records.iter().filter(|r| r.marked_bad).count();
    let flagged_today = store.transcripts_marked_bad_in_last(24).map_err(|e| e.to_string())?;
    let audits = store.recent_audit(1000).map_err(|e| e.to_string())?;
    let mut decisions: BTreeMap<String, usize> = BTreeMap::new();
    for record in &audits {
        *decisions.entry(format!("{:?}", record.decision)).or_default() += 1;
    }

    Ok(json!({
        "wake_word": effective_wake_word(&store, &loaded.config),
        "dictations": records.len(),
        "total_words": total_words,
        "average_words": if records.is_empty() { 0 } else { total_words / records.len() },
        "streak_days": streak,
        "active_days": per_day.len(),
        "today_words": today_words,
        "today_dictations": today_dictations,
        "flagged_total": flagged_total,
        "flagged_today": flagged_today,
        "last_days": last_days,
        "decisions": decisions,
        "commands_run": audits.len(),
        "save_transcripts": loaded.config.history.save_transcripts,
        "keep_days": loaded.config.history.keep_days,
    }))
}

/// The wake word in effect: the one saved in the database wins over the file.
fn effective_wake_word(store: &Store, config: &Config) -> String {
    store
        .get_setting::<String>("wake_word")
        .ok()
        .flatten()
        .filter(|w| !w.trim().is_empty())
        .unwrap_or_else(|| config.wake_word.0.clone())
}

fn history(body: &Value) -> Result<Value, String> {
    let limit = body.get("limit").and_then(Value::as_u64).unwrap_or(300).min(5000) as u32;
    let flagged_only = body.get("flagged_only").and_then(Value::as_bool).unwrap_or(false);
    let store = store()?;
    let records = if flagged_only {
        store.transcripts_marked_bad().map_err(|e| e.to_string())?
    } else {
        store.recent_transcripts(limit).map_err(|e| e.to_string())?
    };
    let dir = harvest_dir();
    // What each reported dictation was reported as ("dictation" / "command").
    let reported: BTreeMap<Uuid, String> =
        store.all_feedback().unwrap_or_default().into_iter().map(|f| (f.transcript_id, f.kind)).collect();
    let rows: Vec<Value> = records
        .iter()
        .map(|r| {
            json!({
                "id": r.id.to_string(),
                "at": r.created_at.to_rfc3339(),
                "day": local_day(r.created_at).to_string(),
                "time": r.created_at.with_timezone(&Local).format("%H:%M").to_string(),
                "raw": r.raw,
                "pre": r.pre_formatted,
                "text": r.formatted,
                "words": words_in(&r.formatted),
                "bad": r.marked_bad,
                "kind": reported.get(&r.id).cloned(),
                "audio": dir.join(format!("{}.wav", r.id)).exists(),
            })
        })
        .collect();
    Ok(json!({ "records": rows, "saving": Config::load().config.history.save_transcripts }))
}

fn flag(body: &Value) -> Result<Value, String> {
    let id = uuid_of(body, "id")?;
    store()?.mark_transcript_bad(id).map_err(|e| e.to_string())?;
    Ok(json!({ "flagged": true }))
}

// ---- the report on a wrong dictation: what it was, what was meant, what to learn ----

fn transcript(store: &Store, id: Uuid) -> Result<eva_store::TranscriptRecord, String> {
    store
        .recent_transcripts(5000)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| "ese dictado ya no está en el historial".to_string())
}

fn feedback_json(f: &eva_store::Feedback) -> Value {
    json!({
        "kind": f.kind,
        "intended": f.intended,
        "causes": serde_json::from_str::<Value>(&f.causes).unwrap_or_else(|_| json!([])),
        "note": f.note,
        "words": serde_json::from_str::<Value>(&f.words).unwrap_or_else(|_| json!([])),
        "updated": f.updated_at.with_timezone(&Local).format("%d/%m %H:%M").to_string(),
    })
}

/// The dictation and what was said about it before, to open the report form.
fn feedback_get(body: &Value) -> Result<Value, String> {
    let id = uuid_of(body, "id")?;
    let store = store()?;
    let r = transcript(&store, id)?;
    let before = store.get_feedback(id).map_err(|e| e.to_string())?;
    Ok(json!({
        "id": id.to_string(),
        "time": r.created_at.with_timezone(&Local).format("%H:%M").to_string(),
        "raw": r.raw, "pre": r.pre_formatted, "text": r.formatted,
        "audio": harvest_dir().join(format!("{id}.wav")).exists(),
        "before": before.as_ref().map(feedback_json),
    }))
}

/// Compares what came out with what was meant, word by word.
fn feedback_analyze(body: &Value) -> Result<Value, String> {
    let id = uuid_of(body, "id")?;
    let intended = text_of(body, "intended");
    if intended.is_empty() {
        return Err("escribe qué querías decir".to_string());
    }
    let store = store()?;
    let r = transcript(&store, id)?;
    let wake = effective_wake_word(&store, &Config::load().config);
    Ok(json!({
        "edits": crate::analysis::edits(&r.raw, &r.pre_formatted, &r.formatted, intended),
        "wake_variant": crate::analysis::wake_variant(&wake, &r.formatted, intended),
        "wake_word": wake,
    }))
}

/// Saves the report: marks the dictation, keeps the expected text next to
/// its audio for the eval corpus, and teaches what the user confirmed.
fn feedback_save(body: &Value) -> Result<Value, String> {
    let id = uuid_of(body, "id")?;
    let kind = match text_of(body, "kind") {
        "command" => "command",
        _ => "dictation",
    };
    let intended = text_of(body, "intended");
    if intended.is_empty() {
        return Err("escribe qué querías decir".to_string());
    }
    let store = store()?;
    transcript(&store, id)?;
    store.mark_transcript_bad(id).map_err(|e| e.to_string())?;
    let dir = harvest_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{id}.txt")), format!("{intended}\n")).map_err(|e| e.to_string())?;

    let edits = body.get("words").and_then(Value::as_array).cloned().unwrap_or_default();
    let (mut corrections, mut dictionary) = (0u32, 0u32);
    for edit in &edits {
        let heard = edit.get("heard").and_then(Value::as_str).unwrap_or("").trim();
        let meant = edit.get("meant").and_then(Value::as_str).unwrap_or("").trim();
        let flag = |name: &str| edit.get(name).and_then(Value::as_bool).unwrap_or(false);
        if flag("learn") && !heard.is_empty() && !meant.is_empty() {
            let key = crate::analysis::key_of(heard);
            if !key.is_empty() && key != crate::analysis::key_of(meant) {
                store.learn_correction(&key, meant).map_err(|e| e.to_string())?;
                corrections += 1;
            }
        }
        if flag("dict") && !meant.is_empty() {
            store.add_custom_word(meant).map_err(|e| e.to_string())?;
            dictionary += 1;
        }
    }
    let wake = body.get("wake_variant").and_then(Value::as_str).map(str::trim).filter(|w| !w.is_empty());
    if let Some(variant) = wake {
        store.teach_wake_variant(&crate::analysis::key_of(variant), 3).map_err(|e| e.to_string())?;
    }

    let now = Utc::now();
    store
        .save_feedback(&eva_store::Feedback {
            transcript_id: id,
            kind: kind.to_string(),
            intended: intended.to_string(),
            causes: body.get("causes").cloned().unwrap_or_else(|| json!([])).to_string(),
            note: text_of(body, "note").to_string(),
            words: Value::Array(edits).to_string(),
            created_at: now,
            updated_at: now,
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({ "saved": true, "corrections": corrections, "dictionary": dictionary, "wake": wake }))
}

/// What reports taught, to show and undo.
fn corrections() -> Result<Value, String> {
    let store = store()?;
    let rows: Vec<Value> = store
        .list_corrections()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(heard, meant, hits)| json!({ "heard": heard, "meant": meant, "hits": hits }))
        .collect();
    let wake = Config::load().config.wake_word.0;
    Ok(
        json!({ "corrections": rows, "wake_variants": store.trusted_wake_variants(3).unwrap_or_default(), "wake_word": wake }),
    )
}

fn corrections_forget(body: &Value) -> Result<Value, String> {
    let heard = text_of(body, "heard");
    let store = store()?;
    if body.get("wake").and_then(Value::as_bool).unwrap_or(false) {
        // A wake spelling is forgotten by teaching it nothing: reset its count.
        store.forget_wake_variant(heard).map_err(|e| e.to_string())?;
    } else {
        store.forget_correction(heard).map_err(|e| e.to_string())?;
    }
    corrections()
}

// ---- developer: which models are running, and what they could be ----

/// The speech, formatting and app-resolving models: which is in use, what
/// else is installed, and what the local server offers.
fn models() -> Result<Value, String> {
    use eva_config::models::{discover, model_dir, ModelChoice, MODELS};
    let config = Config::load().config;
    let support = support_dir();
    let choice = discover(&config, &support, &|key| std::env::var(key).ok());
    let (kind, active) = match &choice {
        ModelChoice::Canary(path) => ("canary", Some(path.clone())),
        ModelChoice::Whisper(path) => ("whisper", Some(path.clone())),
        ModelChoice::None => ("none", None),
    };
    let stt: Vec<Value> = MODELS
        .iter()
        .map(|spec| {
            let dir = model_dir(&support, spec.id);
            json!({
                "id": spec.id, "description": spec.description, "installed": spec.is_installed(&dir),
                "path": dir.display().to_string(), "bytes": spec.total_bytes(), "active": active.as_ref() == Some(&dir),
            })
        })
        .collect();
    let second_dir = model_dir(&support, "canary-180m-flash");
    let second_installed = eva_config::models::find("canary-180m-flash").is_some_and(|s| s.is_installed(&second_dir));
    let second_active =
        config.stt.second_opinion && second_installed && active.as_ref() != Some(&second_dir) && kind == "canary";

    let resolver = eva_text::AppResolver::new(&config.resolver.base_url, &config.resolver.model);
    let local = resolver.is_some();
    let installed = resolver.as_ref().and_then(eva_text::AppResolver::installed_models);
    let installed_json = installed.as_ref().map(|list| {
        list.iter().map(|m| json!({ "name": m.name, "bytes": m.bytes, "loaded": m.loaded })).collect::<Vec<_>>()
    });
    let model_ready = installed.as_ref().is_some_and(|list| list.iter().any(|m| m.name == config.resolver.model));

    Ok(json!({
        "stt": {
            "kind": kind, "active_path": active.map(|p| p.display().to_string()),
            "models": stt, "override": config.stt.canary_dir,
            "second_opinion": { "wanted": config.stt.second_opinion, "installed": second_installed, "active": second_active },
            "language": config.stt.language, "padding": config.stt.padding,
        },
        "formatter": {
            "apple_intelligence": eva_text::AppleIntelligenceFormatter::new().is_some(),
            "remote": { "enabled": config.remote.enabled, "model": config.remote.model, "base_url": config.remote.base_url, "use_for": config.remote.use_for },
        },
        "resolver": {
            "enabled": config.resolver.enabled, "base_url": config.resolver.base_url, "model": config.resolver.model,
            "local_address": local, "running": installed_json.is_some(), "installed": installed_json, "model_ready": model_ready,
            "suggested": ["qwen2.5:3b", "qwen2.5:1.5b", "llama3.2:3b"],
        },
    }))
}

/// Asks the configured resolver about a name, with this Mac's real apps, and
/// says what it answered and how long it took.
fn resolver_test(body: &Value) -> Result<Value, String> {
    let heard = text_of(body, "heard");
    if heard.is_empty() {
        return Err("escribe cómo suena (breve, zafarí…)".to_string());
    }
    let config = Config::load().config;
    let resolver = eva_text::AppResolver::new(&config.resolver.base_url, &config.resolver.model)
        .ok_or("la dirección del resolvedor no es de este Mac")?;
    let apps = eva_macos::installed_apps();
    let started = std::time::Instant::now();
    let answer = resolver.resolve(heard, &apps);
    Ok(
        json!({ "answer": answer, "ms": started.elapsed().as_millis() as u64, "apps": apps.len(), "model": config.resolver.model }),
    )
}

// ---- dictionary ----

fn dictionary() -> Result<Value, String> {
    let store = store()?;
    // A database made before app aliases existed has no such table: the
    // dictionary still works, the aliases list is just empty.
    let aliases: Vec<Value> = store
        .list_app_aliases()
        .unwrap_or_default()
        .into_iter()
        .map(|(heard, app)| json!({ "heard": heard, "app": app }))
        .collect();
    Ok(json!({ "words": store.list_custom_words().map_err(|e| e.to_string())?, "aliases": aliases }))
}

fn dictionary_change(body: &Value, add: bool) -> Result<Value, String> {
    let word = text_of(body, "word");
    if word.is_empty() {
        return Err("escribe una palabra".to_string());
    }
    let store = store()?;
    if add {
        store.add_custom_word(word).map_err(|e| e.to_string())?;
    } else {
        store.remove_custom_word(word).map_err(|e| e.to_string())?;
    }
    dictionary()
}

fn alias(body: &Value, add: bool) -> Result<Value, String> {
    let heard = text_of(body, "heard");
    if heard.is_empty() {
        return Err("escribe cómo suena".to_string());
    }
    let store = store()?;
    if add {
        let app = text_of(body, "app");
        if app.is_empty() {
            return Err("escribe el nombre de la app".to_string());
        }
        store.learn_app_alias(heard, app).map_err(|e| e.to_string())?;
    } else {
        store.forget_app_alias(heard).map_err(|e| e.to_string())?;
    }
    dictionary()
}

// ---- commands: what can be said, the user's own, what ran ----

/// One item of a command's `open` list as the wizard shows it: an app, a web
/// address, or one of the verbs (`buscar:`, `youtube:`, `reproducir:`,
/// `pausar:`, `siguiente:`, `anterior:`).
fn step_of(item: &str) -> Value {
    use eva_intent::Step;
    match Step::parse(item) {
        Step::Search(value) => json!({ "kind": "search", "value": value }),
        Step::Youtube(value) => json!({ "kind": "youtube", "value": value }),
        Step::Play(value) => json!({ "kind": "play", "value": value.unwrap_or_default() }),
        Step::Pause => json!({ "kind": "pause", "value": "" }),
        Step::Next => json!({ "kind": "next", "value": "" }),
        Step::Previous => json!({ "kind": "previous", "value": "" }),
        Step::Open(value) => {
            let is_address = value.contains("://") || (value.contains('.') && !value.contains(' '));
            json!({ "kind": if is_address { "url" } else { "app" }, "value": value })
        }
    }
}

fn describe_command(command: &CommandConfig, learned: &[eva_store::command_phrases::LearnedPhrase]) -> Value {
    let (kind, target, error) = match command.action() {
        Ok(CommandAction::Insert(text)) => ("insert", json!(text), Value::Null),
        Ok(CommandAction::Open(items)) => ("open", json!(items), Value::Null),
        Ok(CommandAction::Task(prompt)) => ("task", json!(prompt), Value::Null),
        Err(why) => ("invalid", Value::Null, json!(why)),
    };
    let origin = command.origin().to_string();
    let steps: Vec<Value> =
        if kind == "open" { command.open.iter().map(|item| step_of(item)).collect() } else { Vec::new() };
    let taught: Vec<Value> = learned
        .iter()
        .filter(|l| l.command == command.say)
        .map(|l| json!({ "phrase": l.phrase, "how": l.how, "at": l.learned_at }))
        .collect();
    json!({
        "say": command.say,
        "also": command.also,
        "kind": kind,
        "target": target,
        "steps": steps,
        "learned": taught,
        "error": error,
        "origin": origin,
        "editable": origin.starts_with("commands/ui-"),
    })
}

fn commands() -> Result<Value, String> {
    let loaded = Config::load();
    let builtins: Vec<Value> = eva_intent::catalog::BUILTINS
        .iter()
        .map(|b| json!({ "say": b.say, "does": b.does, "example": b.example, "kind": b.kind }))
        .collect();
    let mut apps = eva_macos::installed_apps();
    apps.sort_by_key(|name| name.to_lowercase());
    let store = store().ok();
    let wake_word =
        store.as_ref().map(|s| effective_wake_word(s, &loaded.config)).unwrap_or(loaded.config.wake_word.0.clone());
    let learned = store.as_ref().and_then(|s| s.list_command_phrases().ok()).unwrap_or_default();
    Ok(json!({
        "wake_word": wake_word,
        "builtins": builtins,
        "custom": loaded.config.commands.iter().map(|c| describe_command(c, &learned)).collect::<Vec<_>>(),
        "apps": apps,
        "warnings": loaded.warnings,
    }))
}

/// A file-name-safe version of what the command is called.
fn slug(text: &str) -> String {
    let folded: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' => 'a',
            'é' | 'è' | 'ë' => 'e',
            'í' | 'ì' | 'ï' => 'i',
            'ó' | 'ò' | 'ö' => 'o',
            'ú' | 'ù' | 'ü' => 'u',
            'ñ' => 'n',
            other => other,
        })
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let joined = folded.split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-");
    joined.chars().take(40).collect()
}

fn strings(body: &Value, key: &str) -> Vec<String> {
    body.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
        })
        .unwrap_or_default()
}

/// The file name (inside `commands/`) the panel may write or delete: only its
/// own `ui-*.toml`, never `config.toml`, never a file the user made by hand.
fn ui_file(file: &str) -> Option<&str> {
    let name = file.strip_prefix("commands/")?;
    let safe = name.starts_with("ui-") && name.ends_with(".toml") && !name.contains('/') && !name.contains("..");
    safe.then_some(name)
}

fn commands_save(body: &Value) -> Result<Value, String> {
    let say = text_of(body, "say");
    let mut command = CommandConfig { say: say.to_string(), ..CommandConfig::default() };
    // Other ways of saying it: no repeats, none equal to the main phrase.
    for phrase in strings(body, "also") {
        let norm = eva_intent::intent::normalize_phrase(&phrase);
        let seen = command.phrases().any(|p| eva_intent::intent::normalize_phrase(p) == norm);
        if !norm.is_empty() && !seen {
            command.also.push(phrase);
        }
    }
    match text_of(body, "kind") {
        "insert" => command.insert = Some(body.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
        "open" => command.open = strings(body, "open"),
        "task" => command.task = Some(text_of(body, "text").to_string()),
        _ => return Err("elige qué hace: pegar un texto, abrir algo o dar una tarea a un agente".to_string()),
    }
    command.action()?;

    let name = slug(say);
    if name.is_empty() {
        return Err("la frase necesita al menos una letra o un número".to_string());
    }
    let file = format!("commands/ui-{name}.toml");
    let replacing = match text_of(body, "replace") {
        "" => None,
        old => Some(ui_file(old).map(|_| old).ok_or("solo se pueden editar las órdenes creadas desde el panel")?),
    };

    // A phrase means one thing: none of these may already be another command's.
    let loaded = Config::load();
    for phrase in command.phrases() {
        let owner = loaded
            .config
            .commands
            .iter()
            .filter(|c| Some(c.origin()) != replacing)
            .find(|c| c.phrases().any(|other| eva_intent::intent::is_phrase(phrase, other)));
        if let Some(owner) = owner {
            return Err(format!("«{phrase}» ya es una forma de decir la orden «{}»", owner.say));
        }
    }
    if replacing != Some(file.as_str()) && Config::commands_dir().join(format!("ui-{name}.toml")).exists() {
        return Err(format!("ya tienes una orden con un nombre parecido a «{say}»: ábrela y edítala"));
    }

    let dir = Config::commands_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("no se pudo crear {}: {e}", dir.display()))?;
    let path = dir.join(format!("ui-{name}.toml"));

    #[derive(serde::Serialize)]
    struct File<'a> {
        commands: Vec<&'a CommandConfig>,
    }
    let toml_body = toml::to_string(&File { commands: vec![&command] }).map_err(|e| e.to_string())?;
    // An empty `open = []` on a command that pastes text only confuses whoever edits the file.
    let text =
        format!("# Creada desde el panel de EVA01. Puedes editarla a mano.\n{}", toml_body.replace("open = []\n", ""));
    std::fs::write(&path, text).map_err(|e| format!("no se pudo escribir {}: {e}", path.display()))?;

    // Renamed: the old file goes, and what was learned for it follows the new name.
    if let Some(old) = replacing.filter(|old| *old != file) {
        let old_say = loaded.config.commands.iter().find(|c| c.origin() == old).map(|c| c.say.clone());
        if let Some(old_name) = ui_file(old) {
            let _ = std::fs::remove_file(Config::commands_dir().join(old_name));
        }
        if let (Some(old_say), Ok(store)) = (old_say, store()) {
            let _ = store.rename_command_phrases(&old_say, say);
        }
    }
    let loaded = Config::load();
    Ok(json!({ "file": file, "warnings": loaded.warnings }))
}

fn commands_delete(body: &Value) -> Result<Value, String> {
    let file = text_of(body, "file");
    let name = ui_file(file).ok_or("solo se pueden borrar las órdenes creadas desde el panel")?;
    let say = Config::load().config.commands.iter().find(|c| c.origin() == file).map(|c| c.say.clone());
    std::fs::remove_file(Config::commands_dir().join(name)).map_err(|e| format!("no se pudo borrar: {e}"))?;
    if let (Some(say), Ok(store)) = (say, store()) {
        let _ = store.forget_command_phrases_of(&say);
    }
    Ok(json!({ "deleted": file }))
}

/// Forgets one phrase EVA learned by itself, and answers with the commands as
/// they are now.
fn commands_forget_phrase(body: &Value) -> Result<Value, String> {
    let phrase = text_of(body, "phrase");
    if phrase.is_empty() {
        return Err("falta qué olvidar".to_string());
    }
    store()?.forget_command_phrase(phrase).map_err(|e| e.to_string())?;
    commands()
}

/// Ways of saying `name` that people use, for the wizard to offer as cards.
/// Deterministic on purpose: a suggestion is a starting point, not a guess
/// the user has to double-check.
fn suggest_phrases(name: &str) -> Vec<String> {
    let name = eva_intent::intent::normalize_phrase(name);
    let (first, rest) = name.split_once(' ').unwrap_or((name.as_str(), ""));
    if first.is_empty() {
        return Vec::new();
    }
    let is_infinitive = first.len() >= 3 && ["ar", "er", "ir"].iter().any(|end| first.ends_with(end));
    let mut variants: Vec<String> = vec![format!("quiero {name}"), format!("necesito {name}"), format!("hazme {name}")];
    if is_infinitive && !rest.is_empty() {
        variants.push(format!("vamos a {name}"));
        variants.push(format!("me gustaría {name}"));
        match first {
            "ver" => {
                variants.extend([
                    format!("muéstrame {rest}"),
                    format!("pon {rest}"),
                    format!("abre {rest}"),
                    format!("ponme {rest}"),
                ]);
            }
            "abrir" => variants.extend([format!("abre {rest}"), format!("ábreme {rest}")]),
            "escuchar" | "reproducir" | "poner" => {
                variants.extend([format!("pon {rest}"), format!("ponme {rest}"), format!("reproduce {rest}")]);
            }
            "mostrar" => variants.extend([format!("muéstrame {rest}"), format!("muestra {rest}")]),
            _ => {}
        }
    } else {
        variants.extend([
            format!("activa {name}"),
            format!("pon {name}"),
            format!("ponme {name}"),
            format!("abre {name}"),
            format!("ejecuta {name}"),
            format!("lanza {name}"),
            format!("dame {name}"),
        ]);
    }
    let mut seen = std::collections::BTreeSet::new();
    variants.retain(|v| v != &name && seen.insert(eva_intent::intent::normalize_phrase(v)));
    variants.truncate(8);
    variants
}

fn commands_suggest(body: &Value) -> Result<Value, String> {
    let name = text_of(body, "name");
    if name.is_empty() {
        return Err("escribe primero cómo se llama la orden".to_string());
    }
    Ok(json!({ "phrases": suggest_phrases(name) }))
}

/// Says the command out loud to a fresh worker, for real: what the wizard's
/// "Probar ahora" does. A step that needs a confirmation is refused here
/// (nobody is at a terminal); it asks when said out loud.
fn commands_run(body: &Value) -> Result<Value, String> {
    let say = text_of(body, "say");
    if say.is_empty() {
        return Err("falta la frase".to_string());
    }
    let loaded = Config::load();
    if !loaded.config.custom_commands().any(|c| c.phrases().any(|p| p == say)) {
        return Err(format!("«{say}» no es una orden que pueda ejecutarse"));
    }
    let wake = store().map(|s| effective_wake_word(&s, &loaded.config)).unwrap_or(loaded.config.wake_word.0.clone());
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let output = std::process::Command::new(exe)
        .args(["intent", "--run", &format!("{wake}, {say}")])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("no se pudo ejecutar: {e}"))?;
    let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
    out.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(json!({ "ok": output.status.success(), "output": out.trim() }))
}

/// What EVA would understand from `text` — the same path as dictation, run
/// through `eva intent` (which never executes without `--run`).
fn commands_test(body: &Value) -> Result<Value, String> {
    let text = text_of(body, "text");
    if text.is_empty() {
        return Err("escribe una frase".to_string());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let output = std::process::Command::new(exe)
        .args(["intent", text])
        .output()
        .map_err(|e| format!("no se pudo probar: {e}"))?;
    let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
    out.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(json!({ "output": out.trim() }))
}

// ---- training mode: the user's own recordings, checked against what came out ----

/// The key the worker reads to know whether audio is being kept (the same one
/// `eva-worker`'s `training::SETTING` names).
const TRAINING_SETTING: &str = "training_audio";

fn training_dir() -> PathBuf {
    harvest_dir().with_file_name("training")
}

/// One kept utterance, as the «Revisar» page shows it.
fn read_sample(dir: &std::path::Path, id: &str) -> Option<Value> {
    let sidecar: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{id}.json"))).ok()?).ok()?;
    let wav = std::fs::metadata(dir.join(format!("{id}.wav"))).ok()?;
    let reference = std::fs::read_to_string(dir.join(format!("{id}.txt"))).ok().map(|t| t.trim().to_string());
    let modified =
        wav.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    Some(json!({
        "id": id,
        "at": sidecar["at"],
        "modified": modified,
        "kind": sidecar["kind"],
        "raw": sidecar["raw"],
        "formatted": sidecar["formatted"],
        "seconds": sidecar["seconds"],
        "reference": reference,
        "reviewed": reference.is_some(),
        "bytes": wav.len(),
    }))
}

/// Every kept utterance in `dir`, newest first.
fn read_samples(dir: &std::path::Path) -> Vec<Value> {
    let mut samples: Vec<Value> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let id = path.file_stem()?.to_str()?.to_string();
            (path.extension()? == "wav" && Uuid::parse_str(&id).is_ok()).then(|| read_sample(dir, &id)).flatten()
        })
        .collect();
    samples.sort_by_key(|s| std::cmp::Reverse(s["modified"].as_u64().unwrap_or(0)));
    samples
}

/// How many reviewed samples make the corpus worth measuring on.
const TRAINING_GOAL: usize = 50;

fn training_list(body: &Value) -> Result<Value, String> {
    let dir = training_dir();
    let all = read_samples(&dir);
    let reviewed = all.iter().filter(|s| s["reviewed"] == true).count();
    let bytes: u64 = all.iter().filter_map(|s| s["bytes"].as_u64()).sum();
    let limit = body.get("limit").and_then(Value::as_u64).unwrap_or(80).min(500) as usize;
    let records: Vec<&Value> = all
        .iter()
        .filter(|s| match text_of(body, "filter") {
            "pending" => s["reviewed"] == false,
            "reviewed" => s["reviewed"] == true,
            _ => true,
        })
        .take(limit)
        .collect();
    let config = Config::load().config;
    let enabled = store()
        .ok()
        .and_then(|s| s.get_setting::<bool>(TRAINING_SETTING).ok().flatten())
        .unwrap_or(config.history.save_audio);
    Ok(json!({
        "enabled": enabled,
        "samples": all.len(),
        "reviewed": reviewed,
        "goal": TRAINING_GOAL,
        "bytes": bytes,
        "keep_days": config.history.keep_days,
        "dir": dir.display().to_string(),
        "records": records,
    }))
}

fn training_set(body: &Value) -> Result<Value, String> {
    let enabled = body.get("enabled").and_then(Value::as_bool).ok_or("falta si se activa o no")?;
    store()?.set_setting(TRAINING_SETTING, &enabled).map_err(|e| e.to_string())?;
    training_list(&json!({}))
}

/// The id of a kept sample whose audio exists: only a UUID, never a path.
fn sample_id(body: &Value) -> Result<String, String> {
    let id = text_of(body, "id");
    Uuid::parse_str(id).map_err(|_| "identificador no válido".to_string())?;
    if !training_dir().join(format!("{id}.wav")).is_file() {
        return Err("ese audio ya no está".to_string());
    }
    Ok(id.to_string())
}

/// Writes what was really said, the reference `eva-eval --corpus` reads.
fn write_reference(id: &str, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("escribe lo que dijiste".to_string());
    }
    std::fs::write(training_dir().join(format!("{id}.txt")), format!("{text}\n"))
        .map_err(|e| format!("no se pudo guardar: {e}"))
}

fn training_review(body: &Value) -> Result<Value, String> {
    let id = sample_id(body)?;
    write_reference(&id, text_of(body, "text"))?;
    training_list(&json!({}))
}

/// «Estaba bien»: what was pasted is what was said.
fn training_approve(body: &Value) -> Result<Value, String> {
    let id = sample_id(body)?;
    let sample = read_sample(&training_dir(), &id).ok_or("no se pudo leer ese audio")?;
    let pasted = sample["formatted"].as_str().or_else(|| sample["raw"].as_str()).unwrap_or("").trim().to_string();
    write_reference(&id, &pasted).map_err(|_| "no hubo texto que aprobar: escribe lo que dijiste".to_string())?;
    training_list(&json!({}))
}

fn training_discard(body: &Value) -> Result<Value, String> {
    let id = sample_id(body)?;
    for extension in ["wav", "json", "txt"] {
        let _ = std::fs::remove_file(training_dir().join(format!("{id}.{extension}")));
    }
    training_list(&json!({}))
}

fn audit(body: &Value) -> Result<Value, String> {
    let limit = body.get("limit").and_then(Value::as_u64).unwrap_or(150).min(2000) as u32;
    let store = store()?;
    let transcripts: BTreeMap<Uuid, _> =
        store.recent_transcripts(5000).map_err(|e| e.to_string())?.into_iter().map(|r| (r.id, r)).collect();
    let rows: Vec<Value> = store
        .recent_audit(limit)
        .map_err(|e| e.to_string())?
        .iter()
        .map(|a| {
            let said = a.transcript_id.and_then(|id| transcripts.get(&id));
            json!({
                "id": a.id.to_string(),
                "at": a.created_at.to_rfc3339(),
                "day": local_day(a.created_at).to_string(),
                "time": a.created_at.with_timezone(&Local).format("%H:%M:%S").to_string(),
                "decision": format!("{:?}", a.decision),
                "intent": a.intent_json,
                "result": a.result_summary,
                "heard": said.map(|t| t.raw.clone()),
                "final": said.map(|t| t.formatted.clone()),
            })
        })
        .collect();
    Ok(json!({ "records": rows }))
}

/// The agent tasks, running first and then the recent ones, with how each ended.
fn tasks() -> Result<Value, String> {
    let records = store()?.recent_tasks(200).map_err(|e| e.to_string())?;
    let rows: Vec<Value> = records
        .iter()
        .map(|t| {
            let seconds = t.finished_at.unwrap_or_else(Utc::now).signed_duration_since(t.started_at).num_seconds();
            json!({
                "id": t.id.to_string(),
                "provider": t.provider_id,
                "prompt": t.prompt,
                "project": t.project_dir,
                "branch": t.branch,
                "day": local_day(t.started_at).to_string(),
                "time": t.started_at.with_timezone(&Local).format("%H:%M").to_string(),
                "state": match t.success { None => "running", Some(true) => "ok", Some(false) => "failed" },
                "seconds": seconds.max(0),
                "summary": t.summary,
            })
        })
        .collect();
    Ok(json!({ "tasks": rows }))
}

// ---- configuration ----

fn config_get() -> Result<Value, String> {
    let loaded = Config::load();
    let mut values = serde_json::to_value(&loaded.config).map_err(|e| e.to_string())?;
    if let Some(map) = values.as_object_mut() {
        map.remove("commands");
    }
    let gateway: Vec<Value> = ActionKind::ALL
        .iter()
        .map(|kind| {
            let ask = |origin| serde_json::to_value(loaded.config.gateway.policy(origin, *kind)).unwrap_or(Value::Null);
            json!({
                "kind": kind.name(),
                "voice": ask(Origin::Voice),
                "agent": ask(Origin::Agent),
                "voice_overridden": loaded.config.gateway.voice.contains_key(kind),
                "agent_overridden": loaded.config.gateway.agent.contains_key(kind),
            })
        })
        .collect();
    let wake_word =
        store().map(|s| effective_wake_word(&s, &loaded.config)).unwrap_or(loaded.config.wake_word.0.clone());
    Ok(json!({
        "values": values,
        "wake_word": wake_word,
        "gateway": gateway,
        "confirm_timeout_secs": loaded.config.gateway.confirm_timeout_secs,
        "path": loaded.path.display().to_string(),
        "warnings": loaded.warnings,
    }))
}

#[derive(Clone, Copy)]
enum Kind {
    Str,
    OptStr,
    Bool,
    Int,
    OptInt,
    StrList,
}

/// Every setting the panel may write, and its type. Anything else is refused:
/// the page cannot make this function edit an arbitrary key.
const SETTINGS: &[(&str, Kind)] = &[
    ("hotkey.dictation", Kind::Str),
    ("hotkey.confirm", Kind::Str),
    ("hotkey.cancel", Kind::Str),
    ("hotkey.flag_bad", Kind::Str),
    ("stt.canary_dir", Kind::OptStr),
    ("stt.language", Kind::Str),
    ("stt.second_opinion", Kind::Bool),
    ("stt.padding", Kind::Str),
    ("agents.priority", Kind::StrList),
    ("agents.worktree", Kind::Bool),
    ("agents.default_project", Kind::OptStr),
    ("agents.project_roots", Kind::StrList),
    ("feedback.voice", Kind::Str),
    ("feedback.speak_task_results", Kind::Bool),
    ("feedback.notify_task_results", Kind::Bool),
    ("feedback.task_timeout_secs", Kind::Int),
    ("feedback.send_animation_ms", Kind::Int),
    ("dictation.trailing_space", Kind::Bool),
    ("dictation.pause_media", Kind::Bool),
    ("history.save_transcripts", Kind::Bool),
    ("history.keep_days", Kind::Int),
    ("resolver.enabled", Kind::Bool),
    ("resolver.base_url", Kind::Str),
    ("resolver.model", Kind::Str),
    ("remote.enabled", Kind::Bool),
    ("remote.base_url", Kind::Str),
    ("remote.api_key_env", Kind::Str),
    ("remote.model", Kind::Str),
    ("remote.use_for", Kind::StrList),
    ("gateway.confirm_timeout_secs", Kind::OptInt),
];

/// Sets `key` to `item`. An existing key is kept as it is (with the comment
/// written above it) and only its value changes; `insert` would replace the
/// key and lose that comment.
fn put(table: &mut Table, key: &str, item: Item) {
    match table.get_mut(key) {
        Some(existing) => *existing = item,
        None => {
            table.insert(key, item);
        }
    }
}

/// The table at `path` inside `doc`, created if it is missing.
fn table_at<'a>(doc: &'a mut Document, path: &[&str]) -> Result<&'a mut Table, String> {
    let mut table = doc.as_table_mut();
    for segment in path {
        let item = table.entry(segment).or_insert_with(|| {
            let mut created = Table::new();
            created.set_implicit(true);
            Item::Table(created)
        });
        table = item.as_table_mut().ok_or_else(|| format!("«{segment}» en config.toml no es una sección"))?;
    }
    Ok(table)
}

fn to_item(kind: Kind, key: &str, input: &Value) -> Result<Option<Item>, String> {
    let bad = |what: &str| format!("{key}: se esperaba {what}");
    Ok(match kind {
        Kind::Str => Some(value(input.as_str().ok_or_else(|| bad("texto"))?.trim())),
        Kind::OptStr => match input.as_str().map(str::trim) {
            Some("") | None => None,
            Some(text) => Some(value(text)),
        },
        Kind::Bool => Some(value(input.as_bool().ok_or_else(|| bad("sí o no"))?)),
        Kind::Int => Some(value(input.as_i64().filter(|n| *n >= 0).ok_or_else(|| bad("un número"))?)),
        Kind::OptInt => match input {
            Value::Null => None,
            Value::String(text) if text.trim().is_empty() => None,
            other => Some(value(
                other
                    .as_i64()
                    .or_else(|| other.as_str().and_then(|t| t.trim().parse().ok()))
                    .filter(|n| *n >= 0)
                    .ok_or_else(|| bad("un número"))?,
            )),
        },
        Kind::StrList => {
            let items = input.as_array().ok_or_else(|| bad("una lista"))?;
            let mut array = Array::new();
            for item in items {
                let text = item.as_str().ok_or_else(|| bad("una lista de textos"))?.trim();
                if !text.is_empty() {
                    array.push(text);
                }
            }
            Some(value(array))
        }
    })
}

fn config_set(body: &Value) -> Result<Value, String> {
    let changes = body.get("changes").and_then(Value::as_object).ok_or("no hay cambios")?;
    let path = Config::default_path();
    Config::ensure_file(&path).map_err(|e| format!("no se pudo crear {}: {e}", path.display()))?;
    let original = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut doc: Document = original.parse().map_err(|e| format!("config.toml no se puede leer: {e}"))?;

    if let Some(url) = changes.get("resolver.base_url").and_then(Value::as_str) {
        if eva_text::AppResolver::new(url, "").is_none() {
            return Err("solo se aceptan direcciones de este Mac: http://127.0.0.1:… o http://localhost:…".to_string());
        }
    }
    for (key, input) in changes {
        if let Some((_, kind)) = SETTINGS.iter().find(|(name, _)| name == key) {
            let (section, leaf) = key.rsplit_once('.').ok_or("clave no válida")?;
            let segments: Vec<&str> = section.split('.').collect();
            let table = table_at(&mut doc, &segments)?;
            match to_item(*kind, key, input)? {
                Some(item) => put(table, leaf, item),
                None => {
                    table.remove(leaf);
                }
            }
        } else if let Some((origin, kind)) = key
            .strip_prefix("gateway.")
            .and_then(|rest| rest.split_once('.'))
            .filter(|(o, _)| ["voice", "agent"].contains(o))
        {
            if !ActionKind::ALL.iter().any(|k| k.name() == kind) {
                return Err(format!("acción desconocida: {kind}"));
            }
            let table = table_at(&mut doc, &["gateway", origin])?;
            match input.as_str() {
                Some(policy @ ("auto" | "confirm" | "block")) => {
                    put(table, kind, value(policy));
                }
                Some("default" | "") | None => {
                    table.remove(kind);
                }
                Some(other) => return Err(format!("{key}: «{other}» no existe (auto, confirm, block)")),
            }
        } else if key == "styles.apps" {
            let rules = input.as_array().ok_or("styles.apps: se esperaba una lista")?;
            let mut tables = ArrayOfTables::new();
            for rule in rules {
                let (bundle, style) = (text_of(rule, "bundle_id"), text_of(rule, "style"));
                if bundle.is_empty() {
                    continue;
                }
                let mut entry = Table::new();
                entry.insert("bundle_id", value(bundle));
                entry.insert("style", value(style));
                tables.push(entry);
            }
            table_at(&mut doc, &["styles"])?.insert("apps", Item::ArrayOfTables(tables));
        } else {
            return Err(format!("no se puede cambiar «{key}» desde el panel"));
        }
    }

    let text = doc.to_string();
    let config = Config::parse(&text).map_err(|e| format!("con ese cambio config.toml quedaría inválido: {e}"))?;
    // Values that parse but cannot work ("padding = bogus") are refused when
    // this change is what introduces them; problems the file already had are
    // reported, not blamed on this save.
    let before: Vec<String> = Config::parse(&original).map(|c| c.validate()).unwrap_or_default();
    let introduced: Vec<String> = config.validate().into_iter().filter(|w| !before.contains(w)).collect();
    if !introduced.is_empty() {
        return Err(format!("no se guardó: {}", introduced.join("; ")));
    }
    if text != original {
        // A copy of what was there, so a change that turns out wrong is one `mv` away.
        let _ = std::fs::write(path.with_extension("toml.bak"), &original);
        std::fs::write(&path, &text).map_err(|e| format!("no se pudo guardar: {e}"))?;
    }
    Ok(json!({ "saved": text != original, "warnings": config.validate(), "restart_needed": true }))
}

fn wake_word_set(body: &Value) -> Result<Value, String> {
    let word = text_of(body, "word");
    if word.is_empty() {
        return Err("la palabra de activación no puede estar vacía".to_string());
    }
    store()?.set_setting("wake_word", &word).map_err(|e| e.to_string())?;
    Ok(json!({ "saved": true, "restart_needed": true }))
}

// ---- calibration ----

fn calibration() -> Result<Value, String> {
    let store = store()?;
    let dir = harvest_dir();
    let flagged = store.transcripts_marked_bad().map_err(|e| e.to_string())?;
    let rows: Vec<Value> = flagged
        .iter()
        .map(|r| {
            let stem = r.id.to_string();
            json!({
                "id": stem,
                "at": r.created_at.to_rfc3339(),
                "day": local_day(r.created_at).to_string(),
                "time": r.created_at.with_timezone(&Local).format("%H:%M").to_string(),
                "raw": r.raw,
                "text": r.formatted,
                "audio": dir.join(format!("{stem}.wav")).exists(),
                "expected": std::fs::read_to_string(dir.join(format!("{stem}.txt"))).ok().map(|t| t.trim().to_string()),
                "feedback": store.get_feedback(r.id).ok().flatten().as_ref().map(feedback_json),
            })
        })
        .collect();
    let config = Config::load().config;
    Ok(json!({
        "flagged": rows,
        "flagged_today": store.transcripts_marked_bad_in_last(24).map_err(|e| e.to_string())?,
        "harvest_dir": dir.display().to_string(),
        "padding": config.stt.padding,
        "second_opinion": config.stt.second_opinion,
        "language": config.stt.language,
    }))
}

/// Saves what the user says the dictation should have been, next to its
/// audio, as `<id>.txt`: the file that turns the pair into a corpus sample
/// for `eva-eval` (`eval/README.md`).
fn calibration_correct(body: &Value) -> Result<Value, String> {
    let id = uuid_of(body, "id")?;
    let expected = text_of(body, "text");
    if expected.is_empty() {
        return Err("escribe cómo debía haber salido".to_string());
    }
    let dir = harvest_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{id}.txt")), format!("{expected}\n")).map_err(|e| e.to_string())?;
    Ok(json!({ "saved": true }))
}

/// Remembers that a guided calibration just finished.
fn calibration_done() -> Result<Value, String> {
    let now = Utc::now().to_rfc3339();
    store()?.set_setting("last_calibration", &now).map_err(|e| e.to_string())?;
    Ok(json!({ "at": now }))
}

/// What EVA01 has learned from the user: spellings of the wake word, apps said
/// wrongly, corrections taught by reports — each one can be forgotten.
fn learned() -> Result<Value, String> {
    let store = store()?;
    let variants: Vec<Value> = store
        .trusted_wake_variants(1)
        .unwrap_or_default()
        .into_iter()
        .map(|word| json!({ "hits": store.count_wake_variant(&word).unwrap_or(0), "word": word }))
        .collect();
    let aliases: Vec<Value> = store
        .list_app_aliases()
        .unwrap_or_default()
        .into_iter()
        .map(|(heard, app)| json!({ "heard": heard, "app": app }))
        .collect();
    let corrections: Vec<Value> = store
        .list_corrections()
        .unwrap_or_default()
        .into_iter()
        .map(|(heard, meant, hits)| json!({ "heard": heard, "meant": meant, "hits": hits }))
        .collect();
    let phrases: Vec<Value> = store
        .list_command_phrases()
        .unwrap_or_default()
        .into_iter()
        .map(|l| json!({ "phrase": l.phrase, "command": l.command, "how": l.how }))
        .collect();
    let last = store.get_setting::<String>("last_calibration").ok().flatten();
    Ok(json!({
        "wake_variants": variants,
        "phrases": phrases,
        "aliases": aliases,
        "corrections": corrections,
        "last_calibration": last,
        "wake_word": effective_wake_word(&store, &Config::load().config),
    }))
}

fn learned_forget(body: &Value) -> Result<Value, String> {
    let key = text_of(body, "key");
    if key.is_empty() {
        return Err("falta qué olvidar".to_string());
    }
    let store = store()?;
    match text_of(body, "kind") {
        "wake" => store.forget_wake_variant(key),
        "alias" => store.forget_app_alias(key),
        "phrase" => store.forget_command_phrase(key),
        "correction" => store.forget_correction(key),
        _ => return Err("no sé qué es eso".to_string()),
    }
    .map_err(|e| e.to_string())?;
    learned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn edit(original: &str, changes: Value) -> Result<String, String> {
        let mut doc: Document = original.parse().unwrap();
        for (key, input) in changes.as_object().unwrap() {
            let (_, kind) = SETTINGS.iter().find(|(name, _)| name == key).ok_or("clave")?;
            let (section, leaf) = key.rsplit_once('.').unwrap();
            let segments: Vec<&str> = section.split('.').collect();
            let table = table_at(&mut doc, &segments)?;
            match to_item(*kind, key, input)? {
                Some(item) => put(table, leaf, item),
                None => {
                    table.remove(leaf);
                }
            }
        }
        Ok(doc.to_string())
    }

    #[test]
    fn a_setting_is_written_without_losing_the_comments_around_it() {
        let original = "# mi configuración\n[stt]\n# el idioma\nlanguage = \"es\"\n";
        let edited = edit(original, json!({ "stt.language": "en", "history.keep_days": 7 })).unwrap();
        assert!(edited.contains("# mi configuración") && edited.contains("# el idioma"), "{edited}");
        let config = Config::parse(&edited).unwrap();
        assert_eq!(config.stt.language, "en");
        assert_eq!(config.history.keep_days, 7);
    }

    #[test]
    fn an_empty_optional_removes_the_key_and_a_wrong_type_is_refused() {
        let edited =
            edit("[agents]\ndefault_project = \"~/code/x\"\n", json!({ "agents.default_project": "" })).unwrap();
        assert_eq!(Config::parse(&edited).unwrap().agents.default_project, None);
        assert!(edit("", json!({ "history.keep_days": "muchos" })).is_err());
        assert!(edit("", json!({ "stt.second_opinion": "sí" })).is_err());
    }

    #[test]
    fn only_listed_settings_can_be_written() {
        assert!(edit("", json!({ "remote.something_else": "x" })).is_err());
    }

    #[test]
    fn a_command_file_name_comes_from_the_phrase() {
        assert_eq!(slug("Mi correo"), "mi-correo");
        assert_eq!(slug("¡Abre   Ñandú!"), "abre-nandu");
        assert_eq!(slug("???"), "");
    }

    #[test]
    fn suggestions_read_like_something_a_person_would_say() {
        let said = suggest_phrases("Ver mi canal favorito");
        for expected in [
            "quiero ver mi canal favorito",
            "muéstrame mi canal favorito",
            "pon mi canal favorito",
            "abre mi canal favorito",
        ] {
            assert!(said.iter().any(|s| s == expected), "{expected}: {said:?}");
        }
        assert!(!said.iter().any(|s| s == "ver mi canal favorito"), "not the phrase itself: {said:?}");

        let noun = suggest_phrases("modo enfoque");
        assert!(noun.contains(&"activa modo enfoque".to_string()), "{noun:?}");
        assert!(noun.len() <= 8);
        assert!(suggest_phrases("   ").is_empty());
    }

    #[test]
    fn kept_samples_are_read_newest_first_with_their_review_state() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, stray) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string(), "no-es-uuid");
        for (id, raw) in [(&a, "hola"), (&b, "")] {
            std::fs::write(dir.path().join(format!("{id}.wav")), [0u8; 64]).unwrap();
            std::fs::write(
                dir.path().join(format!("{id}.json")),
                json!({ "at": "x", "kind": if raw.is_empty() { "empty" } else { "dictation" }, "raw": raw, "formatted": raw, "seconds": 1.0 }).to_string(),
            )
            .unwrap();
        }
        std::fs::write(dir.path().join(format!("{stray}.wav")), [0u8; 4]).unwrap();
        std::fs::write(dir.path().join(format!("{a}.txt")), "Hola.\n").unwrap();

        let samples = read_samples(dir.path());
        assert_eq!(samples.len(), 2, "a file that is not a UUID is not a sample");
        let of = |id: &str| samples.iter().find(|s| s["id"] == id).unwrap();
        assert_eq!((of(&a)["reviewed"].clone(), of(&a)["reference"].clone()), (json!(true), json!("Hola.")));
        assert_eq!((of(&b)["reviewed"].clone(), of(&b)["kind"].clone()), (json!(false), json!("empty")));
    }

    #[test]
    fn only_a_uuid_with_audio_can_be_reviewed_and_an_empty_reference_is_refused() {
        for id in ["", "../../etc/passwd", "no-es-uuid", "00000000-0000-0000-0000-000000000000"] {
            assert!(sample_id(&json!({ "id": id })).is_err(), "{id:?}");
        }
        assert!(write_reference("00000000-0000-0000-0000-000000000000", "   ").is_err());
    }

    #[test]
    fn a_step_is_an_app_an_address_or_a_search() {
        assert_eq!(step_of("Brave"), json!({ "kind": "app", "value": "Brave" }));
        assert_eq!(step_of("https://youtube.com/@mio"), json!({ "kind": "url", "value": "https://youtube.com/@mio" }));
        assert_eq!(step_of("github.com/foo"), json!({ "kind": "url", "value": "github.com/foo" }));
        assert_eq!(step_of("buscar: clima de hoy"), json!({ "kind": "search", "value": "clima de hoy" }));
        assert_eq!(step_of("youtube: música chill"), json!({ "kind": "youtube", "value": "música chill" }));
        assert_eq!(step_of("reproducir: spotify:playlist:37i9dQZF1DXcBWIGoYBM5M")["kind"], "play");
        assert_eq!(step_of("pausar:"), json!({ "kind": "pause", "value": "" }));
        assert_eq!(step_of("siguiente:")["kind"], "next");
        assert_eq!(step_of("Buscar:"), json!({ "kind": "app", "value": "Buscar:" }));
    }

    #[test]
    fn only_files_the_panel_wrote_can_be_deleted() {
        for file in ["config.toml", "commands/mio.toml", "commands/ui-../x.toml", "commands/../ui-x.toml", "ui-x.toml"]
        {
            assert!(commands_delete(&json!({ "file": file })).is_err(), "{file}");
        }
    }
}
