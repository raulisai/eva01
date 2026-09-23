//! The two ends of the agent → worker socket (`eva_ipc::rpc`).
//!
//! [`RemoteService`] is what an agent's `eva-mcp` child uses: it holds no
//! privileges and forwards each call to the `eva-worker` that launched the
//! agent, which rules on it (policy, overlay confirmation, audit) and does
//! it. [`serve`] is the worker's side: it accepts those connections and runs
//! each request through a [`DesktopService`] — a
//! [`LocalService`](crate::service::LocalService) acting for
//! [`Origin::Agent`](eva_config::Origin::Agent).

use crate::service::{DesktopService, Outcome, ServiceError};
use async_trait::async_trait;
use eva_ipc::rpc::{DesktopCall, DesktopOp, DesktopReply, ProjectEntry, WindowInfo};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// How long a call may take end to end. A confirmation waits on a human, so
/// this is generous — the worker's own confirmation timeout (30s by default)
/// is what normally ends a wait, this only stops a dead worker from hanging
/// an agent forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(180);

/// Talks to a running `eva-worker` over its gateway socket.
pub struct RemoteService {
    socket: PathBuf,
    token: String,
}

impl RemoteService {
    /// A client for the socket at `socket`, authenticating with `token`.
    pub fn new(socket: impl Into<PathBuf>, token: impl Into<String>) -> RemoteService {
        RemoteService { socket: socket.into(), token: token.into() }
    }

    /// One request/reply round trip on a fresh connection — a connection per
    /// call keeps this correct across worker restarts, at a cost of a few
    /// microseconds a Unix socket does not notice.
    async fn call(&self, op: DesktopOp) -> Outcome<DesktopReply> {
        let unreachable = |e: &dyn std::fmt::Display| ServiceError::Failed(format!("no se pudo hablar con EVA: {e}"));

        let exchange = async {
            let stream = UnixStream::connect(&self.socket).await.map_err(|e| unreachable(&e))?;
            let (read, mut write) = stream.into_split();

            let line = eva_ipc::encode_line(&DesktopCall { token: self.token.clone(), op })
                .map_err(|e| ServiceError::Failed(format!("no se pudo codificar la petición: {e}")))?;
            write.write_all(line.as_bytes()).await.map_err(|e| unreachable(&e))?;

            let reply = BufReader::new(read)
                .lines()
                .next_line()
                .await
                .map_err(|e| unreachable(&e))?
                .ok_or_else(|| ServiceError::Failed("EVA cerró la conexión sin responder".to_string()))?;
            eva_ipc::decode_line::<DesktopReply>(&reply)
                .map_err(|e| ServiceError::Failed(format!("respuesta ilegible de EVA: {e}")))
        };

        tokio::time::timeout(CALL_TIMEOUT, exchange)
            .await
            .map_err(|_| ServiceError::Failed("EVA no respondió a tiempo".to_string()))?
    }

    async fn call_expecting_done(&self, op: DesktopOp) -> Outcome<()> {
        match self.call(op).await? {
            DesktopReply::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }
}

/// Maps a reply that is not the expected shape: refusals and failures keep
/// their meaning, anything else is a protocol bug worth naming.
fn unexpected(reply: DesktopReply) -> ServiceError {
    match reply {
        DesktopReply::Refused { reason } => ServiceError::Refused(reason),
        DesktopReply::Failed { message } => ServiceError::Failed(message),
        other => ServiceError::Failed(format!("respuesta inesperada de EVA: {other:?}")),
    }
}

#[async_trait]
impl DesktopService for RemoteService {
    async fn open_app(&self, name: &str) -> Outcome<()> {
        self.call_expecting_done(DesktopOp::OpenApp { name: name.to_string() }).await
    }

    async fn close_app(&self, name: &str) -> Outcome<()> {
        self.call_expecting_done(DesktopOp::CloseApp { name: name.to_string() }).await
    }

    async fn open_url(&self, url: &str) -> Outcome<()> {
        self.call_expecting_done(DesktopOp::OpenUrl { url: url.to_string() }).await
    }

    async fn insert_text(&self, text: &str) -> Outcome<()> {
        self.call_expecting_done(DesktopOp::InsertText { text: text.to_string() }).await
    }

    async fn active_window(&self) -> Outcome<Option<WindowInfo>> {
        match self.call(DesktopOp::ActiveWindow).await? {
            DesktopReply::Window { window } => Ok(window),
            other => Err(unexpected(other)),
        }
    }

    async fn notify(&self, title: &str, body: &str) -> Outcome<()> {
        self.call_expecting_done(DesktopOp::Notify { title: title.to_string(), body: body.to_string() }).await
    }

    async fn speak(&self, text: &str) -> Outcome<()> {
        self.call_expecting_done(DesktopOp::Speak { text: text.to_string() }).await
    }

    async fn selected_text(&self) -> Outcome<Option<String>> {
        match self.call(DesktopOp::SelectedText).await? {
            DesktopReply::Text { text } => Ok(text),
            other => Err(unexpected(other)),
        }
    }

    async fn ask_confirmation(&self, question: &str, detail: &str) -> Outcome<bool> {
        match self
            .call(DesktopOp::AskConfirmation { question: question.to_string(), detail: detail.to_string() })
            .await?
        {
            DesktopReply::Confirmed { approved } => Ok(approved),
            other => Err(unexpected(other)),
        }
    }

    async fn list_projects(&self) -> Outcome<Vec<ProjectEntry>> {
        match self.call(DesktopOp::ListProjects).await? {
            DesktopReply::Projects { projects } => Ok(projects),
            other => Err(unexpected(other)),
        }
    }
}

/// Serves gateway requests on `listener` until it is dropped: one task per
/// connection, one request per line, each answered with one reply line.
/// Requests whose token is wrong are refused and the connection closed.
pub async fn serve(listener: UnixListener, token: String, service: Arc<dyn DesktopService>) {
    let token: Arc<str> = Arc::from(token);
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let (token, service) = (Arc::clone(&token), Arc::clone(&service));
                tokio::spawn(async move { handle_connection(stream, &token, service.as_ref()).await });
            }
            Err(e) => {
                tracing::warn!("el socket del gateway dejó de aceptar conexiones: {e}");
                return;
            }
        }
    }
}

async fn handle_connection(stream: UnixStream, token: &str, service: &dyn DesktopService) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let reply = match eva_ipc::decode_line::<DesktopCall>(&line) {
            Ok(call) if constant_time_eq(call.token.as_bytes(), token.as_bytes()) => dispatch(call.op, service).await,
            Ok(_) => {
                tracing::warn!("petición al gateway con un token inválido; se cierra la conexión");
                let _ = write_reply(&mut write, &DesktopReply::Refused { reason: "token inválido".to_string() }).await;
                return;
            }
            Err(e) => DesktopReply::Failed { message: format!("petición ilegible: {e}") },
        };
        if write_reply(&mut write, &reply).await.is_err() {
            return;
        }
    }
}

async fn write_reply(write: &mut tokio::net::unix::OwnedWriteHalf, reply: &DesktopReply) -> std::io::Result<()> {
    let line = eva_ipc::encode_line(reply).map_err(std::io::Error::other)?;
    write.write_all(line.as_bytes()).await?;
    write.flush().await
}

/// Runs one operation through `service` and shapes the result as a reply.
async fn dispatch(op: DesktopOp, service: &dyn DesktopService) -> DesktopReply {
    fn done(outcome: Outcome<()>) -> DesktopReply {
        match outcome {
            Ok(()) => DesktopReply::Done,
            Err(e) => failure(e),
        }
    }
    fn failure(error: ServiceError) -> DesktopReply {
        match error {
            ServiceError::Refused(reason) => DesktopReply::Refused { reason },
            ServiceError::Failed(message) => DesktopReply::Failed { message },
        }
    }

    match op {
        DesktopOp::OpenApp { name } => done(service.open_app(&name).await),
        DesktopOp::CloseApp { name } => done(service.close_app(&name).await),
        DesktopOp::OpenUrl { url } => done(service.open_url(&url).await),
        DesktopOp::InsertText { text } => done(service.insert_text(&text).await),
        DesktopOp::Notify { title, body } => done(service.notify(&title, &body).await),
        DesktopOp::Speak { text } => done(service.speak(&text).await),
        DesktopOp::ActiveWindow => match service.active_window().await {
            Ok(window) => DesktopReply::Window { window },
            Err(e) => failure(e),
        },
        DesktopOp::SelectedText => match service.selected_text().await {
            Ok(text) => DesktopReply::Text { text },
            Err(e) => failure(e),
        },
        DesktopOp::AskConfirmation { question, detail } => match service.ask_confirmation(&question, &detail).await {
            Ok(approved) => DesktopReply::Confirmed { approved },
            Err(e) => failure(e),
        },
        DesktopOp::ListProjects => match service.list_projects().await {
            Ok(projects) => DesktopReply::Projects { projects },
            Err(e) => failure(e),
        },
    }
}

/// Compares two byte strings without stopping at the first difference, so
/// the time a wrong token takes says nothing about how much of it was right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use crate::desktop::mock::{Call, MockDesktop};
    use crate::service::{LocalService, ProjectSource};
    use eva_config::{GatewayConfig, Origin};
    use eva_gateway::mock::Scripted;
    use eva_gateway::Gateway;
    use eva_store::Store;

    struct NoProjects;
    impl ProjectSource for NoProjects {
        fn list(&self, _title: Option<&str>) -> Vec<ProjectEntry> {
            vec![ProjectEntry { name: "eva01".into(), path: "/code/eva01".into(), active: false }]
        }
    }

    /// A real listener on a temp socket, served by a real `LocalService`
    /// over a mock desktop — the whole agent → worker path minus the OS UI.
    struct Server {
        remote: RemoteService,
        desktop: Arc<MockDesktop>,
        confirmer: Arc<Scripted>,
        socket: PathBuf,
        _dir: tempfile::TempDir,
    }

    async fn start(confirmer: Scripted, desktop: MockDesktop) -> Server {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("gateway.sock");
        let listener = UnixListener::bind(&socket).expect("bind");

        let desktop = Arc::new(desktop);
        let confirmer = Arc::new(confirmer);
        let gateway = Arc::new(Gateway::new(
            GatewayConfig::default(),
            Store::open_in_memory().expect("store"),
            confirmer.clone(),
        ));
        let service = LocalService::new(
            Origin::Agent,
            gateway,
            desktop.clone(),
            confirmer.clone(),
            Arc::new(NoProjects),
            Duration::from_secs(5),
        );
        tokio::spawn(serve(listener, "secreto".to_string(), Arc::new(service)));

        Server { remote: RemoteService::new(&socket, "secreto"), desktop, confirmer, socket, _dir: dir }
    }

    #[tokio::test]
    async fn a_call_travels_the_socket_and_reaches_the_desktop() {
        let server = start(Scripted::denying(), MockDesktop::new()).await;
        server.remote.open_url("https://github.com").await.expect("allowed");
        assert_eq!(server.desktop.calls(), vec![Call::OpenUrl("https://github.com".into())]);
    }

    #[tokio::test]
    async fn the_gateway_rules_on_what_arrives_over_the_socket() {
        let server = start(Scripted::denying(), MockDesktop::new()).await;
        let result = server.remote.close_app("Spotify").await;
        assert!(matches!(result, Err(ServiceError::Refused(_))), "an agent closing an app needs a yes");
        assert!(server.desktop.calls().is_empty());
        assert_eq!(server.confirmer.questions().len(), 1);
    }

    #[tokio::test]
    async fn a_confirmed_action_over_the_socket_runs() {
        let server = start(Scripted::approving(), MockDesktop::new()).await;
        server.remote.insert_text("hola").await.expect("confirmed");
        assert_eq!(server.desktop.calls(), vec![Call::InsertText("hola".into())]);
    }

    #[tokio::test]
    async fn return_values_survive_the_round_trip() {
        let desktop = MockDesktop::new().with_selection("seleccionado");
        let server = start(Scripted::approving(), desktop).await;
        assert_eq!(server.remote.selected_text().await.expect("ok"), Some("seleccionado".into()));
        assert!(server.remote.ask_confirmation("¿Seguir?", "").await.expect("ok"));
        assert_eq!(server.remote.list_projects().await.expect("ok")[0].name, "eva01");
        assert_eq!(server.remote.active_window().await.expect("ok"), None);
    }

    #[tokio::test]
    async fn a_wrong_token_is_refused_and_nothing_runs() {
        let server = start(Scripted::approving(), MockDesktop::new()).await;
        let impostor = RemoteService::new(&server.socket, "otro-token");
        let result = impostor.open_app("Brave Browser").await;
        assert_eq!(result, Err(ServiceError::Refused("token inválido".to_string())));
        assert!(server.desktop.calls().is_empty());
    }

    #[tokio::test]
    async fn a_missing_socket_is_a_clear_failure_not_a_hang() {
        let remote = RemoteService::new("/definitivamente/no/existe.sock", "t");
        let result = remote.open_app("Brave Browser").await;
        assert!(matches!(result, Err(ServiceError::Failed(m)) if m.contains("no se pudo hablar con EVA")));
    }

    #[tokio::test]
    async fn garbage_on_the_socket_gets_an_error_reply_and_the_server_keeps_serving() {
        let server = start(Scripted::denying(), MockDesktop::new()).await;
        let stream = UnixStream::connect(&server.socket).await.expect("connect");
        let (read, mut write) = stream.into_split();
        write.write_all(b"esto no es json\n").await.expect("write");
        let line = BufReader::new(read).lines().next_line().await.expect("read").expect("a reply");
        let reply: DesktopReply = eva_ipc::decode_line(&line).expect("decodes");
        assert!(matches!(reply, DesktopReply::Failed { .. }));

        server.remote.open_app("Brave Browser").await.expect("the server must still work");
    }

    #[test]
    fn constant_time_eq_agrees_with_plain_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
