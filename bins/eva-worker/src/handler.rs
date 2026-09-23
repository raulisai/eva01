//! Turns one [`ShellToWorker`] command into work. Nothing here waits for
//! anything slow: recording control and the small commands run inline, and
//! everything that takes real time — transcription, formatting, an agent
//! task, a confirmation the user has not answered yet — is spawned as a
//! tracked job, so the loop that calls this is already reading the next
//! command. That is what lets `Cancel`, `ConfirmationResponse` and a fresh
//! `StartRecording` arrive while an agent works or the overlay is asking.

use crate::context::WorkerContext;
use crate::{dictation, housekeeping, recording};
use eva_ipc::{ShellToWorker, WorkerState};
use std::sync::Arc;

/// Handles one command. Returns immediately; results arrive on
/// `ctx.events`.
pub fn handle(ctx: &Arc<WorkerContext>, command: ShellToWorker, gateway_socket: Option<&str>) {
    match command {
        ShellToWorker::RunIntentText { request_id, text } => {
            ctx.events.state(request_id, WorkerState::Thinking);
            let job_ctx = Arc::clone(ctx);
            ctx.spawn_job(async move { dictation::process_text(&job_ctx, request_id, &text).await });
        }
        ShellToWorker::InterpretText { request_id, text } => dictation::interpret_text(ctx, request_id, &text),
        ShellToWorker::StartRecording { request_id } => recording::start(ctx, request_id),
        ShellToWorker::StopRecording { request_id } => recording::stop(ctx, request_id),
        ShellToWorker::Cancel { request_id } => {
            // A request id names either a recording in progress or a running
            // agent task; a cancel for neither (already finished) just
            // settles the overlay.
            if !recording::cancel(ctx, request_id) && ctx.tasks.cancel(request_id) {
                return; // the task reports its own end
            }
            ctx.events.state(request_id, WorkerState::Idle);
        }
        ShellToWorker::CancelAllTasks => {
            let cancelled = ctx.tasks.cancel_all();
            tracing::info!(cancelled, "tareas canceladas desde la bandeja");
        }
        ShellToWorker::ListTasks { request_id } => housekeeping::list_tasks(ctx, request_id),
        ShellToWorker::FlagLastDictation { request_id } => housekeeping::flag_last_dictation(ctx, request_id),
        ShellToWorker::ConfirmationResponse { confirmation_id, approved } => {
            if !ctx.broker.resolve(confirmation_id, approved) {
                tracing::info!(%confirmation_id, "respuesta a una confirmación que ya no esperaba; se ignora");
            }
        }
        ShellToWorker::HealthCheck { request_id } => {
            housekeeping::health(ctx, request_id, gateway_socket.map(str::to_string))
        }
        ShellToWorker::AddCustomWord { request_id, word } => housekeeping::add_custom_word(ctx, request_id, &word),
        ShellToWorker::RemoveCustomWord { request_id, word } => {
            housekeeping::remove_custom_word(ctx, request_id, &word)
        }
        ShellToWorker::ListCustomWords { request_id } => housekeeping::list_custom_words(ctx, request_id),
        ShellToWorker::SetWakeWord { request_id, word } => housekeeping::set_wake_word(ctx, request_id, &word),
        // `main.rs` owns the shutdown sequence (cancel tasks, drain, exit).
        ShellToWorker::Shutdown => {}
    }
}
