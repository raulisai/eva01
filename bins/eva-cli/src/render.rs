//! How worker events read on a terminal.

use eva_ipc::{TaskInfo, TaskState, WorkerToShell};

/// One line of CLI output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The text.
    pub text: String,
    /// Whether it goes to stderr and makes the exit code non-zero.
    pub is_error: bool,
}

impl Line {
    fn out(text: impl Into<String>) -> Option<Line> {
        Some(Line { text: text.into(), is_error: false })
    }

    /// Prints the line where it belongs.
    pub fn print(&self) {
        if self.is_error {
            eprintln!("{}", self.text);
        } else {
            println!("{}", self.text);
        }
    }
}

/// The line for `event`, if it has anything worth showing on a CLI. Pure, so
/// what a person reads is a unit test.
pub fn render(event: &WorkerToShell) -> Option<Line> {
    match event {
        // Internal bookkeeping; not interesting on a CLI.
        WorkerToShell::Ready
        | WorkerToShell::StateChanged { .. }
        | WorkerToShell::ConfirmationRequested { .. }
        | WorkerToShell::ConfirmationClosed { .. }
        | WorkerToShell::WakeWordHeard { .. }
        | WorkerToShell::AboutToPaste { .. }
        | WorkerToShell::FollowUp { .. }
        | WorkerToShell::Pong { .. } => None,
        WorkerToShell::TextNotPasted { text, reason, .. } => Line::out(format!("{reason}; el texto: {text}")),
        WorkerToShell::DictationFlagged { message, .. } | WorkerToShell::Notice { message, .. } => {
            Line::out(message.clone())
        }
        WorkerToShell::Transcript { raw, cleaned, .. } => {
            Line::out(format!("transcript crudo:     {raw}\ntranscript limpio:    {cleaned}"))
        }
        WorkerToShell::IntentRecognized { intent_json, .. } => Line::out(format!("intent: {intent_json}")),
        WorkerToShell::AgentEvent { event_json, .. } => render_agent_event(event_json),
        WorkerToShell::Error { message, recoverable, .. } => Some(Line {
            text: format!("error{}: {message}", if *recoverable { "" } else { " (fatal)" }),
            is_error: true,
        }),
        WorkerToShell::Health { report, .. } => Line::out(render_health(report)),
        WorkerToShell::CustomWords { words, .. } => Line::out(if words.is_empty() {
            "(el diccionario personal está vacío)".to_string()
        } else {
            words.iter().map(|w| format!("- {w}")).collect::<Vec<_>>().join("\n")
        }),
        WorkerToShell::Ack { .. } => Line::out("listo."),
        WorkerToShell::TaskStarted { provider, prompt, .. } => {
            Line::out(format!("▶ {} empezó: {prompt}", provider_name(provider)))
        }
        WorkerToShell::TaskFinished { success, summary, .. } => Some(Line {
            text: format!("{} {summary}", if *success { "✓ terminó:" } else { "✗ falló:" }),
            is_error: !success,
        }),
        WorkerToShell::TaskList { tasks, .. } => Line::out(render_tasks(tasks)),
    }
}

fn render_agent_event(json: &serde_json::Value) -> Option<Line> {
    let text = |key: &str| json.get(key).and_then(serde_json::Value::as_str).unwrap_or_default();
    match json.get("kind").and_then(serde_json::Value::as_str)? {
        "message" => Line::out(format!("  agente: {}", text("text"))),
        "tool_call" => match text("summary") {
            "" => Line::out(format!("  agente usa {}", text("name"))),
            summary => Line::out(format!("  agente usa {}: {summary}", text("name"))),
        },
        "file_changed" => Line::out(format!("  agente cambió {}", text("path"))),
        "approval_required" => Line::out(format!("  agente pide aprobación: {}", text("description"))),
        // `started`, `session_assigned`, `completed`, `failed`: the task
        // lines above already say it.
        _ => None,
    }
}

fn render_health(report: &eva_ipc::HealthReport) -> String {
    let mut out = format!(
        "modelo de voz:        {}\nformateador:          {}\nbase de datos:        {}\nproyectos conocidos:  {}",
        report.stt_model_id.clone().unwrap_or_else(|| "(ninguno cargado)".to_string()),
        report.formatter,
        if report.store_ok { "ok" } else { "con problemas" },
        report.project_count,
    );
    for (agent, status) in &report.agents {
        out.push_str(&format!("\nagente {agent}: {status}"));
    }
    if let Some(socket) = &report.gateway_socket {
        out.push_str(&format!("\nsocket del gateway:   {socket}"));
    }
    for warning in &report.config_warnings {
        out.push_str(&format!("\naviso de configuración: {warning}"));
    }
    out
}

/// The `eva tasks` table.
pub fn render_tasks(tasks: &[TaskInfo]) -> String {
    if tasks.is_empty() {
        return "(no hay tareas de agente todavía)".to_string();
    }
    tasks
        .iter()
        .map(|t| {
            let mark = match t.state {
                TaskState::Running => "▶",
                TaskState::Succeeded => "✓",
                TaskState::Failed => "✗",
            };
            let summary = t.summary.as_deref().map(|s| format!("\n    → {s}")).unwrap_or_default();
            format!("{mark} {:<7} hace {:<9} {}{summary}", provider_name(&t.provider), age(t.age_secs), t.prompt)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn provider_name(id: &str) -> &str {
    match id {
        "claude_code" => "Claude",
        "codex" => "Codex",
        other => other,
    }
}

fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3_599 => format!("{} min", secs / 60),
        3_600..=86_399 => format!("{} h", secs / 3_600),
        _ => format!("{} d", secs / 86_400),
    }
}

/// The `eva history` list: when, whether it was flagged as wrong, and the
/// text — plus what the speech model heard when the two differ for a flagged
/// one — and how many were flagged in the last day, against the target of
/// `docs/PLAN.md` §7 (fewer than five).
pub fn render_history(
    records: &[eva_store::TranscriptRecord],
    flagged_last_day: u32,
    harvest_dir: &std::path::Path,
) -> String {
    if records.is_empty() {
        return "(todavía no hay dictados en el historial)".to_string();
    }
    let mut lines: Vec<String> = records
        .iter()
        .map(|r| {
            let when = r.created_at.with_timezone(&chrono::Local).format("%d/%m %H:%M");
            let mark = if r.marked_bad { "✗" } else { "✓" };
            let heard =
                if r.marked_bad && r.raw != r.formatted { format!("\n      oído: {}", r.raw) } else { String::new() };
            format!("{when}  {mark}  {}{heard}", r.formatted)
        })
        .collect();
    lines.push(String::new());
    lines
        .push(format!("Marcados como mal transcritos en las últimas 24 h: {flagged_last_day} (la meta es menos de 5)"));
    lines.push(format!("Audio y propuestas de los marcados: {}", harvest_dir.display()));
    lines.join("\n")
}

/// The `eva audit` table: when, what was decided, about what, and how it went.
pub fn render_audit(records: &[eva_store::AuditRecord]) -> String {
    if records.is_empty() {
        return "(el gateway todavía no ha decidido nada)".to_string();
    }
    records
        .iter()
        .map(|r| {
            let when = r.created_at.with_timezone(&chrono::Local).format("%d/%m %H:%M:%S");
            let verdict = match r.decision {
                eva_store::Decision::AutoApproved => "permitido ",
                eva_store::Decision::UserConfirmed => "confirmado",
                eva_store::Decision::UserRejected => "rechazado ",
                eva_store::Decision::Blocked => "bloqueado ",
            };
            let action = r
                .intent_json
                .get("action")
                .and_then(serde_json::Value::as_str)
                .or_else(|| r.intent_json.get("kind").and_then(serde_json::Value::as_str))
                .unwrap_or("?");
            let origin = match r.intent_json.get("origin").and_then(serde_json::Value::as_str) {
                Some("agent") => "agente",
                Some("voice") => "voz   ",
                _ => "      ",
            };
            let subject = r.intent_json.get("subject").and_then(serde_json::Value::as_str).unwrap_or_default();
            let result = r.result_summary.as_deref().map(|s| format!("\n    → {s}")).unwrap_or_default();
            format!("{when}  {verdict}  {origin}  {action} {subject}{result}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a typed answer means yes ("s", "si", "sí", "y", "yes").
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "s" | "si" | "sí" | "y" | "yes")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use uuid::Uuid;

    fn text_of(event: &WorkerToShell) -> String {
        render(event).expect("this event has a line").text
    }

    #[test]
    fn internal_bookkeeping_prints_nothing() {
        let id = Uuid::new_v4();
        assert_eq!(render(&WorkerToShell::Ready), None);
        assert_eq!(
            render(&WorkerToShell::StateChanged { state: eva_ipc::WorkerState::Thinking, request_id: Some(id) }),
            None
        );
        assert_eq!(render(&WorkerToShell::ConfirmationClosed { confirmation_id: id }), None);
    }

    #[test]
    fn a_transcript_shows_raw_and_cleaned() {
        let event =
            WorkerToShell::Transcript { request_id: Uuid::new_v4(), raw: "eh hola".into(), cleaned: "Hola.".into() };
        assert_eq!(text_of(&event), "transcript crudo:     eh hola\ntranscript limpio:    Hola.");
    }

    #[test]
    fn errors_go_to_stderr_and_mark_the_exit_code() {
        let event = WorkerToShell::Error { request_id: None, message: "x".into(), recoverable: true };
        let line = render(&event).unwrap();
        assert!(line.is_error);
        assert_eq!(line.text, "error: x");
        let fatal = WorkerToShell::Error { request_id: None, message: "x".into(), recoverable: false };
        assert_eq!(render(&fatal).unwrap().text, "error (fatal): x");
    }

    #[test]
    fn a_failed_task_is_an_error_a_successful_one_is_not() {
        let id = Uuid::new_v4();
        let ok = render(&WorkerToShell::TaskFinished { request_id: id, success: true, summary: "3 archivos".into() })
            .unwrap();
        assert_eq!((ok.text.as_str(), ok.is_error), ("✓ terminó: 3 archivos", false));
        let bad =
            render(&WorkerToShell::TaskFinished { request_id: id, success: false, summary: "se cayó".into() }).unwrap();
        assert!(bad.is_error);
    }

    #[test]
    fn agent_progress_reads_as_a_log() {
        let id = Uuid::new_v4();
        let event = |json| WorkerToShell::AgentEvent { request_id: id, event_json: json };
        assert_eq!(text_of(&event(serde_json::json!({"kind": "message", "text": "listo"}))), "  agente: listo");
        assert_eq!(
            text_of(&event(serde_json::json!({"kind": "tool_call", "name": "shell", "summary": "npm test"}))),
            "  agente usa shell: npm test"
        );
        assert_eq!(
            text_of(&event(serde_json::json!({"kind": "file_changed", "path": "a.rs"}))),
            "  agente cambió a.rs"
        );
        assert_eq!(render(&event(serde_json::json!({"kind": "started"}))), None, "the task lines already say it");
        assert_eq!(render(&event(serde_json::json!({"nope": 1}))), None);
    }

    #[test]
    fn a_tool_call_without_a_summary_does_not_leave_a_dangling_colon() {
        let json = serde_json::json!({"kind": "tool_call", "name": "eva/list_projects", "summary": ""});
        let event = WorkerToShell::AgentEvent { request_id: Uuid::new_v4(), event_json: json };
        assert_eq!(text_of(&event), "  agente usa eva/list_projects");
    }

    #[test]
    fn the_health_report_lists_everything_worth_knowing() {
        let report = eva_ipc::HealthReport {
            stt_model_loaded: true,
            stt_model_id: Some("canary:/x".into()),
            store_ok: true,
            agents: vec![("codex".into(), "listo (0.142)".into())],
            formatter: "apple_intelligence".into(),
            config_warnings: vec!["agents.priority: x".into()],
            gateway_socket: Some("/tmp/g.sock".into()),
            project_count: 4,
        };
        let text = render_health(&report);
        for expected in [
            "canary:/x",
            "apple_intelligence",
            "ok",
            "codex: listo (0.142)",
            "/tmp/g.sock",
            "aviso de configuración: agents.priority: x",
            "4",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn the_task_table_marks_state_age_and_summary() {
        let task = |state, summary: Option<&str>, age_secs| TaskInfo {
            request_id: Uuid::new_v4(),
            provider: "claude_code".into(),
            prompt: "arregla el login".into(),
            state,
            summary: summary.map(str::to_string),
            age_secs,
        };
        let table =
            render_tasks(&[task(TaskState::Running, None, 30), task(TaskState::Succeeded, Some("2 archivos"), 4_000)]);
        assert!(table.contains("▶ Claude  hace 30 s"), "{table}");
        assert!(table.contains("✓ Claude  hace 1 h"), "{table}");
        assert!(table.contains("    → 2 archivos"), "{table}");
        assert_eq!(render_tasks(&[]), "(no hay tareas de agente todavía)");
    }

    #[test]
    fn the_audit_table_shows_decision_origin_action_and_outcome() {
        let record = eva_store::AuditRecord {
            id: Uuid::new_v4(),
            created_at: chrono::Utc::now(),
            transcript_id: None,
            intent_json: serde_json::json!({"action": "close_app", "origin": "agent", "subject": "Spotify"}),
            decision: eva_store::Decision::UserRejected,
            result_summary: Some("no confirmaste, así que no lo hice".into()),
        };
        let text = render_audit(&[record]);
        assert!(text.contains("rechazado"), "{text}");
        assert!(text.contains("agente  close_app Spotify"), "{text}");
        assert!(text.contains("    → no confirmaste"), "{text}");
        assert_eq!(render_audit(&[]), "(el gateway todavía no ha decidido nada)");
    }

    #[test]
    fn history_marks_flagged_dictations_and_shows_what_was_heard() {
        let record = |raw: &str, formatted: &str, marked_bad| eva_store::TranscriptRecord {
            id: Uuid::new_v4(),
            created_at: chrono::Utc::now(),
            raw: raw.into(),
            pre_formatted: raw.into(),
            formatted: formatted.into(),
            marked_bad,
        };
        let text = render_history(
            &[record("hola", "Hola.", false), record("que actualizar", "Que actualizar.", true)],
            1,
            std::path::Path::new("/x/harvest"),
        );
        assert!(text.contains("✓  Hola."), "{text}");
        assert!(text.contains("✗  Que actualizar.\n      oído: que actualizar"), "{text}");
        assert!(text.contains("últimas 24 h: 1"), "{text}");
        assert!(text.contains("/x/harvest"), "{text}");
        assert_eq!(render_history(&[], 0, std::path::Path::new("/x")), "(todavía no hay dictados en el historial)");
    }

    #[test]
    fn yes_is_spanish_or_english_and_anything_else_is_no() {
        for yes in ["s", "S", "sí", "si", " y ", "yes"] {
            assert!(is_yes(yes), "{yes}");
        }
        for no in ["", "n", "no", "quizás", "ok"] {
            assert!(!is_yes(no), "{no}");
        }
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age(5), "5 s");
        assert_eq!(age(120), "2 min");
        assert_eq!(age(7_200), "2 h");
        assert_eq!(age(200_000), "2 d");
    }
}
