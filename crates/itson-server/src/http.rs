//! HTTP endpoints and the event WebSocket. SQLite runs on a dedicated worker;
//! password verification runs on a bounded blocking pool. See
//! `docs/GATEWAY.md` for the API and the session lifetime rules.

use crate::gateway::{Connection, Output, Sessions};
use crate::limits::{Rate, RateLimiter, RetryAfter};
use crate::password::Cost;
use crate::service::{Server, ServiceError};
use crate::sqlite::SqliteStore;
use crate::store::Store;
use crate::token::TokenHash;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::ws::{
    CloseFrame, Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection,
};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, State};
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
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, Notify, Semaphore, mpsc, oneshot};

const WORK_QUEUE_CAPACITY: usize = 64;
const TICK_INTERVAL: Duration = Duration::from_secs(1);
const RESUME_GRACE: Duration = Duration::from_secs(60);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const LIMITER_SWEEP_INTERVAL: Duration = Duration::from_secs(3 * 60);
const SESSION_SWEEP_INTERVAL_MS: u64 = 60 * 60 * 1_000;
const MAX_LIMITER_KEYS: usize = 4_096;
const MAX_PASSWORD_JOBS: usize = 4;

type Job = Box<dyn FnOnce(&mut Server<SqliteStore>) + Send>;

enum Work {
    Run(Job),
    Stop(oneshot::Sender<()>),
}

#[derive(Clone)]
struct Database {
    inner: Arc<DatabaseInner>,
}

struct DatabaseInner {
    sender: mpsc::Sender<Work>,
    stopped: AtomicBool,
}

impl Database {
    async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Server<SqliteStore>) -> Result<T, ServiceError> + Send + 'static,
    ) -> Result<T, ServiceError> {
        if self.inner.stopped.load(Ordering::Acquire) {
            return Err(worker_unavailable());
        }
        let (reply, response) = oneshot::channel();
        self.inner
            .sender
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
    info: Arc<Mutex<ServerInfo>>,
    limits: Arc<Mutex<HttpLimits>>,
    password_jobs: Arc<Semaphore>,
    clock: Clock,
    maintenance: Arc<Maintenance>,
}

struct ServerInfo {
    name: String,
    setup_complete: bool,
    setup_hash: Option<TokenHash>,
    code_to_print: Option<String>,
}

struct Maintenance(tokio::task::AbortHandle);

impl Drop for Maintenance {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A wall-clock origin with monotonic elapsed time for deadlines and buckets.
/// Reading this clock happens on the async side before submitting worker jobs.
#[derive(Clone, Copy)]
struct Clock {
    unix_ms: u64,
    started: tokio::time::Instant,
}

impl Clock {
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            unix_ms: now()?,
            started: tokio::time::Instant::now(),
        })
    }

    fn elapsed_ms(self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn unix_ms(self) -> u64 {
        self.unix_ms.saturating_add(self.elapsed_ms())
    }
}

struct HttpLimits {
    creation: RateLimiter<IpAddr>,
    sign_in_addresses: RateLimiter<IpAddr>,
    sign_in_usernames: RateLimiter<String>,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            creation: RateLimiter::new(Rate::ACCOUNT_CREATION, MAX_LIMITER_KEYS),
            sign_in_addresses: RateLimiter::new(Rate::SIGN_IN, MAX_LIMITER_KEYS),
            sign_in_usernames: RateLimiter::new(Rate::SIGN_IN, MAX_LIMITER_KEYS),
        }
    }
}

impl HttpLimits {
    fn creation(&mut self, address: IpAddr, now: u64) -> Result<(), RetryAfter> {
        self.creation.check(address, now)
    }

    fn sign_in_address(&mut self, address: IpAddr, now: u64) -> Result<(), RetryAfter> {
        self.sign_in_addresses.check(address, now)
    }

    fn sign_in_username(&mut self, username: &str, now: u64) -> Result<(), RetryAfter> {
        // Invalid names share a bucket rather than retaining arbitrarily long
        // attacker-supplied strings. Valid names use the service's normalization.
        let key = itson_domain::content::normalize_username(username).unwrap_or_default();
        self.sign_in_usernames.check(key, now)
    }

    fn sweep(&mut self, now: u64) {
        self.creation.sweep(now);
        self.sign_in_addresses.sweep(now);
        self.sign_in_usernames.sweep(now);
    }
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
        let clock = Clock::new()?;
        let startup_now = clock.unix_ms();
        let (sender, mut receiver) = mpsc::channel::<Work>(WORK_QUEUE_CAPACITY);
        let (initialized, initialization) = oneshot::channel();
        std::thread::Builder::new()
            .name("itson-database".into())
            .spawn(move || {
                let opened = (|| -> Result<_, ServiceError> {
                    let mut store = SqliteStore::open(path)?;
                    store.delete_expired_sessions(startup_now)?;
                    let settings = store.settings()?;
                    let code = if settings.is_none() {
                        Some(crate::token::invite_code().ok_or_else(|| {
                            ServiceError::Internal("random source unavailable".into())
                        })?)
                    } else {
                        None
                    };
                    let info = ServerInfo {
                        name: settings
                            .as_ref()
                            .map_or_else(|| "itson".into(), |s| s.name.clone()),
                        setup_complete: settings.is_some(),
                        setup_hash: code
                            .as_ref()
                            .map(|code| TokenHash::of(&SessionToken::new(code.clone()))),
                        code_to_print: code,
                    };
                    Ok((Server::open(store, cost)?, info))
                })();
                let (mut server, info) = match opened {
                    Ok(opened) => opened,
                    Err(error) => {
                        let _ = initialized.send(Err(error));
                        return;
                    }
                };
                if initialized.send(Ok(info)).is_err() {
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
        let info = initialization.await.map_err(|_| worker_unavailable())??;
        let database = Database {
            inner: Arc::new(DatabaseInner {
                sender,
                stopped: AtomicBool::new(false),
            }),
        };
        let limits = Arc::new(Mutex::new(HttpLimits::default()));
        let maintenance = start_maintenance(&database, &limits, clock);
        Ok(Self {
            database,
            gateway: Arc::new(Mutex::new(GatewayState::default())),
            info: Arc::new(Mutex::new(info)),
            limits,
            password_jobs: Arc::new(Semaphore::new(MAX_PASSWORD_JOBS)),
            clock,
            maintenance,
        })
    }

    /// Whether the initial owner and server settings exist.
    pub async fn setup_complete(&self) -> Result<bool, ServiceError> {
        Ok(self.info.lock().await.setup_complete)
    }

    /// Takes the startup setup code once for the host to display. HTTP clients
    /// must supply it; there is no endpoint that reveals it.
    pub async fn take_setup_code(&self) -> Option<String> {
        self.info.lock().await.code_to_print.take()
    }

    /// Stops accepting database work, finishes previously queued operations,
    /// and waits until the worker has closed the database. Call once after
    /// stopping the HTTP listener; all clones then reject new work.
    pub async fn shutdown(&self) -> Result<(), ServiceError> {
        self.maintenance.0.abort();
        if self.database.inner.stopped.swap(true, Ordering::AcqRel) {
            return Err(worker_unavailable());
        }
        let (reply, response) = oneshot::channel();
        self.database
            .inner
            .sender
            .send(Work::Stop(reply))
            .await
            .map_err(|_| worker_unavailable())?;
        response.await.map_err(|_| worker_unavailable())
    }

    async fn update_connection(
        &self,
        mut connection: Connection,
        frame: Option<ClientFrame>,
        lease: Arc<SocketLease>,
    ) -> Result<(Connection, Vec<Output>), ServiceError> {
        let shared = Arc::clone(&self.gateway);
        let now = self.clock.unix_ms();
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
                let outputs = match frame {
                    Some(frame) => connection.handle(frame, server, &mut gateway.sessions, now),
                    None => connection.tick(server, &mut gateway.sessions, now),
                };
                if let Some(hash) = connection.token_hash() {
                    gateway.claim(hash.clone(), lease);
                }
                gateway.wake_pending();
                Ok((connection, outputs))
            })
            .await
    }
}

fn start_maintenance(
    database: &Database,
    limits: &Arc<Mutex<HttpLimits>>,
    clock: Clock,
) -> Arc<Maintenance> {
    let database = Arc::downgrade(&database.inner);
    let limits = Arc::downgrade(limits);
    let task = tokio::spawn(async move {
        let mut timer = tokio::time::interval_at(
            tokio::time::Instant::now() + LIMITER_SWEEP_INTERVAL,
            LIMITER_SWEEP_INTERVAL,
        );
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_sessions = 0;
        loop {
            timer.tick().await;
            let (Some(inner), Some(limits)) = (database.upgrade(), limits.upgrade()) else {
                break;
            };
            if inner.stopped.load(Ordering::Acquire) {
                break;
            }
            let elapsed = clock.elapsed_ms();
            limits.lock().await.sweep(elapsed);
            if elapsed.saturating_sub(last_sessions) >= SESSION_SWEEP_INTERVAL_MS {
                let now = clock.unix_ms();
                if let Err(error) = (Database { inner })
                    .call(move |server| server.delete_expired_sessions(now))
                    .await
                {
                    eprintln!("Could not remove expired sessions: {error}");
                }
                last_sessions = elapsed;
            }
        }
    });
    Arc::new(Maintenance(task.abort_handle()))
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
    setup_code: Option<String>,
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Result<Json<SetupRequest>, JsonRejection>,
) -> Result<impl IntoResponse, HttpError> {
    state
        .limits
        .lock()
        .await
        .creation(peer.ip(), state.clock.elapsed_ms())
        .map_err(HttpError::rate)?;
    let Json(request) = request.map_err(HttpError::json)?;
    let info = Arc::clone(&state.info);
    let now = state.clock.unix_ms();
    let user_id = state
        .database
        .call(move |server| {
            {
                let info = info.blocking_lock();
                if info.setup_complete {
                    return Err(ServiceError::AlreadySetUp);
                }
                let supplied = request
                    .setup_code
                    .as_ref()
                    .map(|code| TokenHash::of(&SessionToken::new(code.clone())));
                if supplied.is_none() || supplied != info.setup_hash {
                    return Err(ServiceError::Forbidden);
                }
            }
            let user = server.setup(
                &request.server_name,
                &request.username,
                &request.password,
                now,
            )?;
            let mut info = info.blocking_lock();
            info.name = request.server_name.trim().to_owned();
            info.setup_complete = true;
            info.setup_hash = None;
            info.code_to_print = None;
            Ok(user)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(CreatedAccount { user_id })))
}

async fn register(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Result<Json<RegisterRequest>, JsonRejection>,
) -> Result<impl IntoResponse, HttpError> {
    state
        .limits
        .lock()
        .await
        .creation(peer.ip(), state.clock.elapsed_ms())
        .map_err(HttpError::rate)?;
    let Json(request) = request.map_err(HttpError::json)?;
    let now = state.clock.unix_ms();
    let user_id = state
        .database
        .call(move |server| {
            server.register(
                request.invite_code.as_deref(),
                &request.username,
                &request.password,
                now,
            )
        })
        .await?;
    Ok((StatusCode::CREATED, Json(CreatedAccount { user_id })))
}

async fn sign_in(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Result<Json<SignInRequest>, JsonRejection>,
) -> Result<impl IntoResponse, HttpError> {
    state
        .limits
        .lock()
        .await
        .sign_in_address(peer.ip(), state.clock.elapsed_ms())
        .map_err(HttpError::rate)?;
    let Json(request) = request.map_err(HttpError::json)?;
    state
        .limits
        .lock()
        .await
        .sign_in_username(&request.username, state.clock.elapsed_ms())
        .map_err(HttpError::rate)?;
    // Keep the permit inside the blocking closure, so cancelling an HTTP
    // request cannot release capacity while its password job is still running.
    let permit = Arc::clone(&state.password_jobs)
        .try_acquire_owned()
        .map_err(|_| HttpError::rate(RetryAfter { ms: 1_000 }))?;
    let challenge = state
        .database
        .call(move |server| server.prepare_sign_in(&request.username))
        .await?;
    let proof = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        challenge.verify(&request.password)
    })
    .await
    .map_err(|_| worker_unavailable())??;
    let now = state.clock.unix_ms();
    let (token, user) = state
        .database
        .call(move |server| server.finish_sign_in(proof, &request.device, now))
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
    let now = state.clock.unix_ms();
    let messages = state
        .database
        .call(move |server| {
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
    #[serde(skip)]
    retry_after: Option<u64>,
    code: ErrorCode,
    detail: String,
}

impl HttpError {
    fn new(status: StatusCode, code: ErrorCode, detail: &str) -> Self {
        Self {
            status,
            retry_after: None,
            code,
            detail: detail.to_owned(),
        }
    }

    fn rate(retry: RetryAfter) -> Self {
        let mut error = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RateLimited,
            "Too many attempts. Try again shortly.",
        );
        error.retry_after = Some(retry.ms.div_ceil(1_000).max(1));
        error
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
        let retry_after = self.retry_after;
        let mut response = (self.status, Json(self)).into_response();
        if unauthorized {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if let Some(seconds) = retry_after
            && let Ok(value) = HeaderValue::try_from(seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
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
    let name = state.info.lock().await.name.clone();
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
    fn claim(&mut self, hash: TokenHash, lease: Arc<SocketLease>) {
        if let Some(previous) = self.owners.insert(hash, Arc::clone(&lease))
            && !Arc::ptr_eq(&previous, &lease)
        {
            previous.notify.notify_one();
        }
    }

    fn wake_pending(&self) {
        for (hash, owner) in &self.owners {
            if self.sessions.needs_drain(hash) {
                owner.notify.notify_one();
            }
        }
    }

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
        self.wake_pending();
    }
}

struct SocketLease {
    live: AtomicBool,
    notify: Notify,
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
        notify: Notify::new(),
    });
    let _disconnect = Disconnect {
        gateway: Arc::clone(&state.gateway),
        lease: Arc::clone(&lease),
    };
    let mut connection = Connection::opened(name, state.clock.unix_ms());
    let mut timer =
        tokio::time::interval_at(tokio::time::Instant::now() + TICK_INTERVAL, TICK_INTERVAL);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut frames = RateLimiter::new(Rate::GATEWAY_FRAMES, 1);
    let mut frames_swept_at = state.clock.elapsed_ms();
    loop {
        let frame = tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break; };
                if matches!(message, Message::Close(_)) { break; }
                if frames.check((), state.clock.elapsed_ms()).is_err() {
                    end_socket(&mut socket, ErrorCode::RateLimited, "Too many frames. Reconnect after waiting briefly.").await;
                    break;
                }
                match message {
                    Message::Text(text) => {
                        if text.len() > MAX_FRAME_BYTES {
                            end_socket(&mut socket, ErrorCode::FrameTooLarge, "That frame is too large.").await;
                            break;
                        }
                        match serde_json::from_str::<ClientFrame>(&text) {
                            Ok(frame) => Some(frame),
                            Err(_) => {
                                end_socket(&mut socket, ErrorCode::Malformed, "Send a valid JSON client frame.").await;
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
                    Message::Ping(_) | Message::Pong(_) => None,
                }
            }
            _ = lease.notify.notified() => None,
            _ = timer.tick() => {
                let elapsed = state.clock.elapsed_ms();
                if elapsed.saturating_sub(frames_swept_at) >= LIMITER_SWEEP_INTERVAL.as_millis() as u64 {
                    frames.sweep(elapsed);
                    frames_swept_at = elapsed;
                }
                None
            },
        };
        match state
            .update_connection(connection, frame, Arc::clone(&lease))
            .await
        {
            Ok((next, outputs)) => {
                connection = next;
                if !send_outputs(&mut socket, outputs).await {
                    break;
                }
            }
            Err(error) => {
                end_socket(&mut socket, error.code(), &error.to_string()).await;
                break;
            }
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
    // A batch, as well as each individual write, has a deadline. A reader
    // making tiny progress must not postpone housekeeping indefinitely.
    tokio::time::timeout(SEND_TIMEOUT, send_outputs_inner(socket, outputs))
        .await
        .unwrap_or(false)
}

async fn send_outputs_inner(socket: &mut WebSocket, outputs: Vec<Output>) -> bool {
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

#[cfg(test)]
mod hardening_tests;
