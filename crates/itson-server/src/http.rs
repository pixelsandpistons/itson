//! HTTP endpoints and the event WebSocket. SQLite and password hashing run on
//! one dedicated worker thread; async handlers await channel replies. See
//! `docs/GATEWAY.md` for the API and the session lifetime rules.

use crate::gateway::{Connection, Output, Sessions};
use crate::password::Cost;
use crate::service::{Server, ServiceError};
use crate::sqlite::SqliteStore;
use crate::store::Store;
use crate::token::TokenHash;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::ws::{
    CloseFrame, Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection,
};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use itson_protocol::frame::{ClientFrame, ErrorCode, ServerEnvelope, ServerFrame};
use itson_protocol::limits::MAX_FRAME_BYTES;
use itson_protocol::{ChannelId, MessageId, SessionToken, UserId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, mpsc, oneshot};

const WORK_QUEUE_CAPACITY: usize = 64;
const DRAIN_INTERVAL: Duration = Duration::from_millis(25);
const RESUME_GRACE: Duration = Duration::from_secs(60);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

type Job = Box<dyn FnOnce(&mut Server<SqliteStore>) + Send>;

enum Work {
    Run(Job),
    Stop(oneshot::Sender<()>),
}

#[derive(Clone)]
struct Database {
    sender: mpsc::Sender<Work>,
    stopped: Arc<AtomicBool>,
}

impl Database {
    async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Server<SqliteStore>) -> Result<T, ServiceError> + Send + 'static,
    ) -> Result<T, ServiceError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(worker_unavailable());
        }
        let (reply, response) = oneshot::channel();
        self.sender
            .send(Work::Run(Box::new(move |server| {
                if !reply.is_closed() {
                    let _ = reply.send(operation(server));
                }
            })))
            .await
            .map_err(|_| worker_unavailable())?;
        response.await.map_err(|_| worker_unavailable())?
    }
}

/// Shared adapter state. The database connection never leaves its worker.
#[derive(Clone)]
pub struct AppState {
    database: Database,
    gateway: Arc<Mutex<GatewayState>>,
}

impl fmt::Debug for AppState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

impl AppState {
    /// Opens the SQLite file and server on a dedicated thread. Its parent
    /// directory must already exist. Tests may supply a lower password cost.
    pub async fn open(path: impl AsRef<Path>, cost: Cost) -> Result<Self, ServiceError> {
        let path = path.as_ref().to_path_buf();
        let (sender, mut receiver) = mpsc::channel::<Work>(WORK_QUEUE_CAPACITY);
        let (initialized, initialization) = oneshot::channel();
        std::thread::Builder::new()
            .name("itson-database".into())
            .spawn(move || {
                let opened = SqliteStore::open(path)
                    .map_err(ServiceError::from)
                    .and_then(|store| Server::open(store, cost));
                let mut server = match opened {
                    Ok(server) => server,
                    Err(error) => {
                        let _ = initialized.send(Err(error));
                        return;
                    }
                };
                if initialized.send(Ok(())).is_err() {
                    return;
                }
                while let Some(work) = receiver.blocking_recv() {
                    match work {
                        Work::Run(job) => job(&mut server),
                        Work::Stop(reply) => {
                            drop(server);
                            let _ = reply.send(());
                            return;
                        }
                    }
                }
            })
            .map_err(|error| ServiceError::Internal(error.to_string()))?;
        initialization.await.map_err(|_| worker_unavailable())??;
        Ok(Self {
            database: Database {
                sender,
                stopped: Arc::new(AtomicBool::new(false)),
            },
            gateway: Arc::new(Mutex::new(GatewayState::default())),
        })
    }

    /// Whether the initial owner and server settings exist.
    pub async fn setup_complete(&self) -> Result<bool, ServiceError> {
        self.database
            .call(|server| Ok(server.store().settings()?.is_some()))
            .await
    }

    /// Stops accepting database work, finishes previously queued operations,
    /// and waits until the worker has closed the database. Call once after
    /// stopping the HTTP listener; all clones then reject new work.
    pub async fn shutdown(&self) -> Result<(), ServiceError> {
        if self.database.stopped.swap(true, Ordering::AcqRel) {
            return Err(worker_unavailable());
        }
        let (reply, response) = oneshot::channel();
        self.database
            .sender
            .send(Work::Stop(reply))
            .await
            .map_err(|_| worker_unavailable())?;
        response.await.map_err(|_| worker_unavailable())
    }

    async fn handle_frame(
        &self,
        mut connection: Connection,
        frame: ClientFrame,
        lease: Arc<SocketLease>,
    ) -> Result<(Connection, Vec<Output>), ServiceError> {
        let shared = Arc::clone(&self.gateway);
        self.database
            .call(move |server| {
                if !lease.live.load(Ordering::Acquire) {
                    return Ok((connection, Vec::new()));
                }
                // This is a dedicated, non-Tokio thread. Async drains wait on the
                // same mutex without blocking an executor thread during SQLite I/O.
                let mut gateway = shared.blocking_lock();
                if let Some(hash) = connection.token_hash()
                    && !gateway.owns(hash, &lease)
                {
                    return Ok((connection, vec![superseded()]));
                }
                let outputs = connection.handle(frame, server, &mut gateway.sessions, now()?);
                if let Some(hash) = connection.token_hash() {
                    gateway.owners.insert(hash.clone(), lease);
                }
                Ok((connection, outputs))
            })
            .await
    }
}

/// Builds the HTTP API and WebSocket route, without binding a listener.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/setup", post(setup))
        .route("/api/register", post(register))
        .route("/api/sign-in", post(sign_in))
        .route("/api/history", get(history))
        .route("/gateway", get(gateway))
        .fallback(|| async {
            HttpError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, "Not found.")
        })
        .method_not_allowed_fallback(|| async {
            HttpError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                ErrorCode::Malformed,
                "That HTTP method is not available here.",
            )
        })
        .layer(DefaultBodyLimit::max(MAX_FRAME_BYTES))
        .layer(middleware::map_response(
            |mut response: Response| async move {
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                response
            },
        ))
        .with_state(state)
}

// Password-bearing request types deliberately have no Debug implementation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupRequest {
    server_name: String,
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterRequest {
    invite_code: Option<String>,
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignInRequest {
    username: String,
    password: String,
    device: String,
}

#[derive(Serialize)]
struct CreatedAccount {
    user_id: UserId,
}

#[derive(Serialize)]
struct SignedIn {
    token: SessionToken,
    user: itson_protocol::model::User,
}

async fn setup(
    State(state): State<AppState>,
    request: Result<Json<SetupRequest>, JsonRejection>,
) -> Result<impl IntoResponse, HttpError> {
    let Json(request) = request.map_err(HttpError::json)?;
    let user_id = state
        .database
        .call(move |server| {
            server.setup(
                &request.server_name,
                &request.username,
                &request.password,
                now()?,
            )
        })
        .await?;
    Ok((StatusCode::CREATED, Json(CreatedAccount { user_id })))
}

async fn register(
    State(state): State<AppState>,
    request: Result<Json<RegisterRequest>, JsonRejection>,
) -> Result<impl IntoResponse, HttpError> {
    let Json(request) = request.map_err(HttpError::json)?;
    let user_id = state
        .database
        .call(move |server| {
            server.register(
                request.invite_code.as_deref(),
                &request.username,
                &request.password,
                now()?,
            )
        })
        .await?;
    Ok((StatusCode::CREATED, Json(CreatedAccount { user_id })))
}

async fn sign_in(
    State(state): State<AppState>,
    request: Result<Json<SignInRequest>, JsonRejection>,
) -> Result<impl IntoResponse, HttpError> {
    let Json(request) = request.map_err(HttpError::json)?;
    let (token, user) = state
        .database
        .call(move |server| {
            server.sign_in(
                &request.username,
                &request.password,
                &request.device,
                now()?,
            )
        })
        .await?;
    Ok(Json(SignedIn { token, user }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    channel: u64,
    before: Option<u64>,
    limit: Option<usize>,
}

async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<HistoryQuery>, QueryRejection>,
) -> Result<impl IntoResponse, HttpError> {
    let token = bearer(&headers)?;
    let Query(query) = query.map_err(|_| {
        HttpError::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::Malformed,
            "History needs a channel ID and optional before and limit numbers.",
        )
    })?;
    let channel = ChannelId(query.channel);
    let before = query.before.map(MessageId);
    if !channel.is_valid() || before.is_some_and(|id| !id.is_valid()) {
        return Err(HttpError::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidContent,
            "That ID is out of range.",
        ));
    }
    let messages = state
        .database
        .call(move |server| {
            let now = now()?;
            let user = server.authenticate(&token, now)?;
            server.history(user.id, channel, before, query.limit.unwrap_or(50), now)
        })
        .await?;
    Ok(Json(messages))
}

fn bearer(headers: &HeaderMap) -> Result<SessionToken, HttpError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let token = value
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, token)| {
            scheme.eq_ignore_ascii_case("bearer")
                && !token.is_empty()
                && !token.chars().any(char::is_whitespace)
        })
        .map(|(_, token)| SessionToken::new(token.to_owned()));
    token.ok_or_else(|| ServiceError::InvalidSession.into())
}

#[derive(Serialize)]
struct HttpError {
    #[serde(skip)]
    status: StatusCode,
    code: ErrorCode,
    detail: String,
}

impl HttpError {
    fn new(status: StatusCode, code: ErrorCode, detail: &str) -> Self {
        Self {
            status,
            code,
            detail: detail.to_owned(),
        }
    }

    fn json(rejection: JsonRejection) -> Self {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorCode::FrameTooLarge,
                "That request is too large.",
            )
        } else {
            Self::new(
                StatusCode::BAD_REQUEST,
                ErrorCode::Malformed,
                "Send a JSON object with the required fields.",
            )
        }
    }
}

impl From<ServiceError> for HttpError {
    fn from(error: ServiceError) -> Self {
        let status = match error {
            ServiceError::InvalidSession | ServiceError::BadCredentials => StatusCode::UNAUTHORIZED,
            ServiceError::Forbidden | ServiceError::Banned => StatusCode::FORBIDDEN,
            ServiceError::NotFound => StatusCode::NOT_FOUND,
            ServiceError::NotSetUp | ServiceError::AlreadySetUp | ServiceError::UsernameTaken => {
                StatusCode::CONFLICT
            }
            ServiceError::Internal(_) | ServiceError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_REQUEST,
        };
        Self::new(status, error.code(), &error.to_string())
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let unauthorized = self.status == StatusCode::UNAUTHORIZED;
        let mut response = (self.status, Json(self)).into_response();
        if unauthorized {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

async fn gateway(
    State(state): State<AppState>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, HttpError> {
    let upgrade = upgrade.map_err(|_| {
        HttpError::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::Malformed,
            "Open this address with a WebSocket client.",
        )
    })?;
    let name = state
        .database
        .call(|server| {
            Ok(server
                .store()
                .settings()?
                .map_or_else(|| "itson".into(), |s| s.name))
        })
        .await?;
    Ok(upgrade
        .max_frame_size(MAX_FRAME_BYTES)
        .max_message_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move { socket_loop(socket, state, &name).await }))
}

#[derive(Default)]
struct GatewayState {
    sessions: Sessions,
    owners: HashMap<TokenHash, Arc<SocketLease>>,
}

impl GatewayState {
    fn owns(&self, hash: &TokenHash, lease: &Arc<SocketLease>) -> bool {
        self.owners
            .get(hash)
            .is_some_and(|owner| Arc::ptr_eq(owner, lease))
    }

    fn expire(&mut self, lease: &Arc<SocketLease>) {
        let expired: Vec<_> = self
            .owners
            .iter()
            .filter(|(_, owner)| Arc::ptr_eq(owner, lease))
            .map(|(hash, _)| hash.clone())
            .collect();
        for hash in expired {
            self.owners.remove(&hash);
            if let Some(user) = self.sessions.close(&hash) {
                self.sessions.publish_offline(user);
            }
        }
    }
}

struct SocketLease {
    live: AtomicBool,
}

struct Disconnect {
    gateway: Arc<Mutex<GatewayState>>,
    lease: Arc<SocketLease>,
}

impl Drop for Disconnect {
    fn drop(&mut self) {
        self.lease.live.store(false, Ordering::Release);
        let gateway = Arc::clone(&self.gateway);
        let lease = Arc::clone(&self.lease);
        let deadline = tokio::time::Instant::now() + RESUME_GRACE;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                tokio::time::sleep_until(deadline).await;
                gateway.lock().await.expire(&lease);
            });
        }
    }
}

async fn socket_loop(mut socket: WebSocket, state: AppState, name: &str) {
    let lease = Arc::new(SocketLease {
        live: AtomicBool::new(true),
    });
    let _disconnect = Disconnect {
        gateway: Arc::clone(&state.gateway),
        lease: Arc::clone(&lease),
    };
    let mut connection = Connection::new(name);
    let mut timer = tokio::time::interval(DRAIN_INTERVAL);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break; };
                match message {
                    Message::Text(text) => {
                        if text.len() > MAX_FRAME_BYTES {
                            end_socket(&mut socket, ErrorCode::FrameTooLarge, "That frame is too large.").await;
                            break;
                        }
                        let frame = match serde_json::from_str::<ClientFrame>(&text) {
                            Ok(frame) => frame,
                            Err(_) => {
                                end_socket(&mut socket, ErrorCode::Malformed, "Send a valid JSON client frame.").await;
                                break;
                            }
                        };
                        match state.handle_frame(connection, frame, Arc::clone(&lease)).await {
                            Ok((next, outputs)) => {
                                connection = next;
                                if !send_outputs(&mut socket, outputs).await { break; }
                            }
                            Err(error) => {
                                end_socket(&mut socket, error.code(), &error.to_string()).await;
                                break;
                            }
                        }
                    }
                    Message::Binary(_) => {
                        end_socket(&mut socket, ErrorCode::Malformed, "Send JSON text frames on this connection.").await;
                        break;
                    }
                    Message::Close(_) => break,
                    // Axum automatically answers WebSocket pings.
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
            _ = timer.tick() => {}
        }
        if let Some(hash) = connection.token_hash() {
            let envelopes = {
                let mut gateway = state.gateway.lock().await;
                if !gateway.owns(hash, &lease) {
                    drop(gateway);
                    end_socket(
                        &mut socket,
                        ErrorCode::NotAuthenticated,
                        "This session continued on another connection.",
                    )
                    .await;
                    break;
                }
                gateway.sessions.drain(hash)
            };
            if !send_outputs(
                &mut socket,
                envelopes.into_iter().map(Output::Send).collect(),
            )
            .await
            {
                break;
            }
        }
    }
}

async fn send_outputs(socket: &mut WebSocket, outputs: Vec<Output>) -> bool {
    for output in outputs {
        match output {
            Output::Send(envelope) => {
                let text = match serde_json::to_string(&envelope) {
                    Ok(text) => text,
                    Err(_) => {
                        end_socket(
                            socket,
                            ErrorCode::Internal,
                            "The server could not prepare its reply.",
                        )
                        .await;
                        return false;
                    }
                };
                if text.len() > MAX_FRAME_BYTES {
                    end_socket(
                        socket,
                        ErrorCode::FrameTooLarge,
                        "This server reply is too large for this connection.",
                    )
                    .await;
                    return false;
                }
                if !send(socket, Message::Text(text.into())).await {
                    return false;
                }
            }
            Output::Close { code, detail } => {
                end_socket(socket, code, &detail).await;
                return false;
            }
        }
    }
    true
}

async fn send(socket: &mut WebSocket, message: Message) -> bool {
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, socket.send(message)).await,
        Ok(Ok(()))
    )
}

async fn end_socket(socket: &mut WebSocket, code: ErrorCode, detail: &str) {
    let envelope = ServerEnvelope::reply(ServerFrame::Error {
        code,
        nonce: None,
        detail: detail.into(),
    });
    if let Ok(text) = serde_json::to_string(&envelope) {
        let _ = send(socket, Message::Text(text.into())).await;
    }
    let (code, reason) = match code {
        ErrorCode::FrameTooLarge => (1009, "Frame too large."),
        ErrorCode::Malformed => (1007, "Invalid client frame."),
        ErrorCode::Internal => (1011, "Server problem."),
        _ => (1008, "Connection closed."),
    };
    let _ = send(
        socket,
        Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })),
    )
    .await;
}

fn superseded() -> Output {
    Output::Close {
        code: ErrorCode::NotAuthenticated,
        detail: "This session continued on another connection.".into(),
    }
}

fn now() -> Result<u64, ServiceError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ServiceError::Internal("the system clock is before the Unix epoch".into()))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| ServiceError::Internal("the system clock is out of range".into()))
}

fn worker_unavailable() -> ServiceError {
    ServiceError::Internal("the database worker is unavailable".into())
}

#[cfg(test)]
mod tests;
