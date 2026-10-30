#![allow(clippy::unwrap_used)]

use super::*;
use crate::gateway::{
    HANDSHAKE_TIMEOUT_MS, IDLE_TIMEOUT_MS, OUTBOX_CAPACITY, REVALIDATE_INTERVAL_MS,
};
use crate::store::{MemoryStore, Session};
use itson_protocol::model::Presence;
use itson_protocol::version::VersionRange;
use std::future::Future;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use tokio_tungstenite::tungstenite::{self, WebSocket as ClientSocket};

const FAST: Cost = Cost { iterations: 1_000 };
const FICTIONAL_NOW: u64 = 1_800_000_000_000;

fn address(last: u8) -> IpAddr {
    // RFC 5737 documentation addresses, never contacted by these unit tests.
    IpAddr::from([192, 0, 2, last])
}

#[test]
fn creation_bucket_allows_its_burst_then_recovers_one_attempt() {
    let mut limits = HttpLimits::default();
    for _ in 0..Rate::ACCOUNT_CREATION.burst {
        assert!(limits.creation(address(1), 0).is_ok());
    }
    let retry = limits.creation(address(1), 0).unwrap_err();
    assert_eq!(retry.ms, Rate::ACCOUNT_CREATION.ms_per_token);
    assert!(limits.creation(address(2), 0).is_ok());
    assert!(limits.creation(address(1), retry.ms - 1).is_err());
    assert!(limits.creation(address(1), retry.ms).is_ok());
    assert!(limits.creation(address(1), retry.ms).is_err());
}

#[test]
fn sign_in_address_bucket_is_independent_of_other_addresses() {
    let mut limits = HttpLimits::default();
    for _ in 0..Rate::SIGN_IN.burst {
        assert!(limits.sign_in_address(address(1), 0).is_ok());
    }
    let retry = limits.sign_in_address(address(1), 0).unwrap_err();
    assert_eq!(retry.ms, Rate::SIGN_IN.ms_per_token);
    assert!(limits.sign_in_address(address(2), 0).is_ok());
    assert!(limits.sign_in_address(address(1), retry.ms - 1).is_err());
    assert!(limits.sign_in_address(address(1), retry.ms).is_ok());
    assert!(limits.sign_in_address(address(1), retry.ms).is_err());
}

#[test]
fn gateway_bucket_has_a_per_connection_burst_and_refills_after_the_wait() {
    let mut first = RateLimiter::new(Rate::GATEWAY_FRAMES, 1);
    let mut second = RateLimiter::new(Rate::GATEWAY_FRAMES, 1);
    for _ in 0..Rate::GATEWAY_FRAMES.burst {
        first.check((), 0).unwrap();
    }
    let retry = first.check((), 0).unwrap_err();
    assert_eq!(retry.ms, Rate::GATEWAY_FRAMES.ms_per_token);
    assert!(second.check((), 0).is_ok());
    assert!(first.check((), retry.ms - 1).is_err());
    assert!(first.check((), retry.ms).is_ok());
    assert!(first.check((), retry.ms).is_err());
}

#[test]
fn username_bucket_normalizes_case_and_whitespace_across_addresses() {
    let mut limits = HttpLimits::default();
    let variants = ["moon_moth", "Moon_Moth", "  MOON_MOTH  ", "\tmoon_moth\n"];
    for attempt in 0..Rate::SIGN_IN.burst {
        assert!(
            limits
                .sign_in_address(address(attempt as u8 + 1), 0)
                .is_ok()
        );
        assert!(
            limits
                .sign_in_username(variants[attempt as usize % variants.len()], 0)
                .is_ok()
        );
    }
    assert!(limits.sign_in_address(address(100), 0).is_ok());
    let retry = limits.sign_in_username(" Moon_Moth ", 0).unwrap_err();
    assert_eq!(retry.ms, Rate::SIGN_IN.ms_per_token);
    assert!(limits.sign_in_username("star_beetle", 0).is_ok());
    assert!(limits.sign_in_username("MOON_MOTH", retry.ms - 1).is_err());
    assert!(limits.sign_in_username("moon_moth", retry.ms).is_ok());
    assert!(limits.sign_in_username(" moon_moth ", retry.ms).is_err());
}

#[test]
fn limiter_sweep_retains_empty_buckets_until_they_fully_recover() {
    let mut limits = HttpLimits::default();
    for _ in 0..Rate::ACCOUNT_CREATION.burst {
        limits.creation(address(1), 0).unwrap();
    }
    for _ in 0..Rate::SIGN_IN.burst {
        limits.sign_in_address(address(1), 0).unwrap();
        limits.sign_in_username("moon_moth", 0).unwrap();
    }
    let creation_refill =
        u64::from(Rate::ACCOUNT_CREATION.burst) * Rate::ACCOUNT_CREATION.ms_per_token;
    let sign_in_refill = u64::from(Rate::SIGN_IN.burst) * Rate::SIGN_IN.ms_per_token;
    let full_refill = creation_refill.max(sign_in_refill);
    limits.sweep(full_refill - 1);
    assert_eq!(limits.creation.tracked_keys(), 1);
    assert_eq!(limits.sign_in_addresses.tracked_keys(), 1);
    assert_eq!(limits.sign_in_usernames.tracked_keys(), 1);
    limits.sweep(full_refill);
    assert_eq!(limits.creation.tracked_keys(), 0);
    assert_eq!(limits.sign_in_addresses.tracked_keys(), 0);
    assert_eq!(limits.sign_in_usernames.tracked_keys(), 0);
}

fn lease() -> Arc<SocketLease> {
    Arc::new(SocketLease {
        live: AtomicBool::new(true),
        notify: Notify::new(),
    })
}

struct NotificationFixture {
    server: Server<MemoryStore>,
    gateway: GatewayState,
    owner: UserId,
    owner_hash: TokenHash,
    owner_connection: Connection,
    owner_lease: Arc<SocketLease>,
    observer_hash: TokenHash,
    observer_lease: Arc<SocketLease>,
}

fn authenticate_memory(
    server: &mut Server<MemoryStore>,
    sessions: &mut Sessions,
    token: SessionToken,
) -> Connection {
    let mut connection = Connection::opened("Fictional moon garden", FICTIONAL_NOW);
    connection.handle(
        ClientFrame::Hello {
            client: "Fictional desktop".into(),
            versions: VersionRange::SUPPORTED,
        },
        server,
        sessions,
        FICTIONAL_NOW,
    );
    let outputs = connection.handle(
        ClientFrame::Authenticate { token },
        server,
        sessions,
        FICTIONAL_NOW,
    );
    assert!(matches!(
        outputs.first(),
        Some(Output::Send(ServerEnvelope {
            frame: ServerFrame::Ready { .. },
            ..
        }))
    ));
    connection
}

fn notification_fixture() -> NotificationFixture {
    let mut server = Server::open(MemoryStore::new(), FAST).unwrap();
    let owner = server
        .setup(
            "Fictional moon garden",
            "moon_moth",
            "fictional garden password",
            FICTIONAL_NOW,
        )
        .unwrap();
    let invite = server
        .create_invite(owner, None, None, FICTIONAL_NOW)
        .unwrap();
    server
        .register(
            Some(&invite.code),
            "star_beetle",
            "fictional observer password",
            FICTIONAL_NOW,
        )
        .unwrap();
    let (owner_token, _) = server
        .sign_in(
            "moon_moth",
            "fictional garden password",
            "Fictional desktop",
            FICTIONAL_NOW,
        )
        .unwrap();
    let (observer_token, _) = server
        .sign_in(
            "star_beetle",
            "fictional observer password",
            "Fictional observer",
            FICTIONAL_NOW,
        )
        .unwrap();
    let owner_hash = TokenHash::of(&owner_token);
    let observer_hash = TokenHash::of(&observer_token);
    let mut gateway = GatewayState::default();
    let owner_connection = authenticate_memory(&mut server, &mut gateway.sessions, owner_token);
    authenticate_memory(&mut server, &mut gateway.sessions, observer_token);
    gateway.sessions.drain(&owner_hash);
    gateway.sessions.drain(&observer_hash);
    let owner_lease = lease();
    let observer_lease = lease();
    gateway.claim(owner_hash.clone(), Arc::clone(&owner_lease));
    gateway.claim(observer_hash.clone(), Arc::clone(&observer_lease));
    NotificationFixture {
        server,
        gateway,
        owner,
        owner_hash,
        owner_connection,
        owner_lease,
        observer_hash,
        observer_lease,
    }
}

async fn expect_notification(lease: &SocketLease) {
    tokio::time::timeout(Duration::from_millis(1), lease.notify.notified())
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn publication_wakes_a_quiet_session_and_the_permit_survives_until_it_waits() {
    let mut fixture = notification_fixture();
    let frame = ServerFrame::PresenceUpdated {
        user: fixture.owner,
        presence: Presence::Idle,
    };
    fixture
        .gateway
        .sessions
        .publish(&[fixture.owner], &frame, None);
    fixture.gateway.wake_pending();
    expect_notification(&fixture.owner_lease).await;
    assert!(fixture.gateway.sessions.needs_drain(&fixture.owner_hash));
    let envelopes = fixture.gateway.sessions.drain(&fixture.owner_hash);
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].frame, frame);
    assert!(!fixture.gateway.sessions.needs_drain(&fixture.owner_hash));
    assert!(
        tokio::time::timeout(Duration::ZERO, fixture.owner_lease.notify.notified())
            .await
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn overflow_wakes_the_reader_even_when_the_outbox_is_empty() {
    let mut fixture = notification_fixture();
    let frame = ServerFrame::PresenceUpdated {
        user: fixture.owner,
        presence: Presence::Idle,
    };
    for _ in 0..=OUTBOX_CAPACITY {
        fixture
            .gateway
            .sessions
            .publish(&[fixture.owner], &frame, None);
    }
    assert!(
        fixture
            .gateway
            .sessions
            .drain(&fixture.owner_hash)
            .is_empty()
    );
    assert!(fixture.gateway.sessions.needs_drain(&fixture.owner_hash));
    fixture.gateway.wake_pending();
    expect_notification(&fixture.owner_lease).await;
    let outputs = fixture.owner_connection.tick(
        &fixture.server,
        &mut fixture.gateway.sessions,
        FICTIONAL_NOW + 1,
    );
    assert!(matches!(
        outputs.first(),
        Some(Output::Close {
            code: ErrorCode::TooSlow,
            ..
        })
    ));
    assert!(!fixture.gateway.sessions.needs_drain(&fixture.owner_hash));
}

#[tokio::test(start_paused = true)]
async fn expiry_wakes_another_members_quiet_socket_for_the_offline_event() {
    let fixture = notification_fixture();
    let gateway = Arc::new(Mutex::new(fixture.gateway));
    let observer = Arc::clone(&fixture.observer_lease);
    let waiting = tokio::spawn(async move { observer.notify.notified().await });
    tokio::task::yield_now().await;
    drop(Disconnect {
        gateway: Arc::clone(&gateway),
        lease: fixture.owner_lease,
    });
    tokio::time::advance(RESUME_GRACE).await;
    tokio::time::timeout(Duration::from_millis(1), waiting)
        .await
        .unwrap()
        .unwrap();
    let mut gateway = gateway.lock().await;
    let envelopes = gateway.sessions.drain(&fixture.observer_hash);
    assert_eq!(envelopes.len(), 1);
    assert_eq!(
        envelopes[0].frame,
        ServerFrame::PresenceUpdated {
            user: fixture.owner,
            presence: Presence::Offline
        }
    );
}

#[tokio::test(start_paused = true)]
async fn takeover_wakes_the_old_owner_and_routes_new_publications_to_the_replacement() {
    let mut fixture = notification_fixture();
    let replacement = lease();
    fixture
        .gateway
        .claim(fixture.owner_hash.clone(), Arc::clone(&replacement));
    expect_notification(&fixture.owner_lease).await;
    assert!(
        !fixture
            .gateway
            .owns(&fixture.owner_hash, &fixture.owner_lease)
    );
    assert!(fixture.gateway.owns(&fixture.owner_hash, &replacement));
    fixture.gateway.sessions.publish(
        &[fixture.owner],
        &ServerFrame::PresenceUpdated {
            user: fixture.owner,
            presence: Presence::Idle,
        },
        None,
    );
    fixture.gateway.wake_pending();
    expect_notification(&replacement).await;
    assert!(
        tokio::time::timeout(Duration::ZERO, fixture.owner_lease.notify.notified())
            .await
            .is_err()
    );
}

#[derive(Debug)]
struct TestDatabase {
    directory: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "itson-hardening-tests-{}-{ordinal}",
                std::process::id()
            ));
            match std::fs::create_dir(&directory) {
                Ok(()) => return Self { directory },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("could not create isolated test directory: {error}"),
            }
        }
        panic!("could not choose an isolated test directory");
    }

    fn path(&self) -> PathBuf {
        self.directory.join("fictional-server.sqlite3")
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

// A database or OS socket worker must finish real work while virtual time is
// paused. Keep this task runnable so Tokio cannot auto-advance unrelated timers.
async fn stay_awake<T>(future: impl Future<Output = T>) -> T {
    tokio::pin!(future);
    loop {
        tokio::select! {
            biased;
            result = &mut future => return result,
            _ = tokio::task::yield_now() => {}
        }
    }
}

struct RunningServer {
    address: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl RunningServer {
    async fn start(state: AppState) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router(state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
        });
        Self {
            address,
            stop: Some(stop),
            task,
        }
    }

    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        stay_awake(&mut self.task).await.unwrap().unwrap();
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn connect(address: SocketAddr) -> ClientSocket<TcpStream> {
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    tungstenite::client(format!("ws://{address}/gateway"), stream)
        .unwrap()
        .0
}

fn read_envelope(client: &mut ClientSocket<TcpStream>) -> ServerEnvelope {
    match client.read().unwrap() {
        tungstenite::Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected a JSON envelope, received {other:?}"),
    }
}

fn assert_error_and_close(mut client: ClientSocket<TcpStream>, code: ErrorCode) {
    match read_envelope(&mut client).frame {
        ServerFrame::Error {
            code: actual,
            detail,
            ..
        } => {
            assert_eq!(actual, code);
            assert!(!detail.is_empty());
        }
        other => panic!("expected an error envelope, received {other:?}"),
    }
    match client.read().unwrap() {
        tungstenite::Message::Close(Some(frame)) => assert_eq!(u16::from(frame.code), 1008),
        other => panic!("expected a policy close, received {other:?}"),
    }
}

#[tokio::test]
async fn stalled_handshake_closes_the_actual_socket_with_not_authenticated() {
    let database = TestDatabase::new();
    let state = AppState::open(database.path(), FAST).await.unwrap();
    let server = RunningServer::start(state.clone()).await;
    let address = server.address;
    let client = tokio::task::spawn_blocking(move || connect(address))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(HANDSHAKE_TIMEOUT_MS + 1)).await;
    stay_awake(tokio::task::spawn_blocking(move || {
        assert_error_and_close(client, ErrorCode::NotAuthenticated)
    }))
    .await
    .unwrap();
    server.stop().await;
    stay_awake(state.shutdown()).await.unwrap();
}

#[tokio::test]
async fn missing_heartbeats_close_the_actual_socket_with_heartbeat_timeout() {
    let database = TestDatabase::new();
    let state = AppState::open(database.path(), FAST).await.unwrap();
    let now = state.clock.unix_ms();
    let token = state
        .database
        .call(move |server| {
            server.setup(
                "Fictional moon garden",
                "moon_moth",
                "fictional garden password",
                now,
            )?;
            Ok(server
                .sign_in(
                    "moon_moth",
                    "fictional garden password",
                    "Fictional desktop",
                    now,
                )?
                .0)
        })
        .await
        .unwrap();
    let server = RunningServer::start(state.clone()).await;
    let address = server.address;
    let client = tokio::task::spawn_blocking(move || {
        let mut client = connect(address);
        let hello = ClientFrame::Hello {
            client: "Fictional desktop".into(),
            versions: VersionRange::SUPPORTED,
        };
        client
            .send(tungstenite::Message::Text(
                serde_json::to_string(&hello).unwrap().into(),
            ))
            .unwrap();
        assert!(matches!(
            read_envelope(&mut client).frame,
            ServerFrame::Welcome { .. }
        ));
        client
            .send(tungstenite::Message::Text(
                serde_json::to_string(&ClientFrame::Authenticate { token })
                    .unwrap()
                    .into(),
            ))
            .unwrap();
        assert!(matches!(
            read_envelope(&mut client).frame,
            ServerFrame::Ready { .. }
        ));
        client
    })
    .await
    .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(IDLE_TIMEOUT_MS + 1)).await;
    stay_awake(tokio::task::spawn_blocking(move || {
        assert_error_and_close(client, ErrorCode::HeartbeatTimeout)
    }))
    .await
    .unwrap();
    server.stop().await;
    stay_awake(state.shutdown()).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn worker_tick_revalidates_a_revoked_session_using_the_adapter_clock() {
    let database = TestDatabase::new();
    let state = stay_awake(AppState::open(database.path(), FAST))
        .await
        .unwrap();
    let now = state.clock.unix_ms();
    let (token, user) = stay_awake(state.database.call(move |server| {
        server.setup(
            "Fictional moon garden",
            "moon_moth",
            "fictional garden password",
            now,
        )?;
        server.sign_in(
            "moon_moth",
            "fictional garden password",
            "Fictional desktop",
            now,
        )
    }))
    .await
    .unwrap();
    let hash = TokenHash::of(&token);
    let socket_lease = lease();
    let mut connection = Connection::opened("Fictional moon garden", state.clock.unix_ms());
    for frame in [
        ClientFrame::Hello {
            client: "Fictional desktop".into(),
            versions: VersionRange::SUPPORTED,
        },
        ClientFrame::Authenticate { token },
    ] {
        let (next, outputs) =
            stay_awake(state.update_connection(connection, Some(frame), Arc::clone(&socket_lease)))
                .await
                .unwrap();
        assert!(
            !outputs
                .iter()
                .any(|output| matches!(output, Output::Close { .. }))
        );
        connection = next;
    }
    let now = state.clock.unix_ms();
    stay_awake(
        state
            .database
            .call(move |server| server.sign_out(user.id, &hash, now)),
    )
    .await
    .unwrap();
    tokio::time::advance(Duration::from_millis(REVALIDATE_INTERVAL_MS)).await;
    let (_, outputs) = stay_awake(state.update_connection(connection, None, socket_lease))
        .await
        .unwrap();
    assert!(matches!(
        outputs.first(),
        Some(Output::Close {
            code: ErrorCode::InvalidToken,
            ..
        })
    ));
    stay_awake(state.shutdown()).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn startup_and_hourly_maintenance_remove_expired_sqlite_sessions() {
    let database = TestDatabase::new();
    let baseline = now().unwrap();
    let expired = TokenHash::of(&SessionToken::new("fictional-expired-token".into()));
    let due = TokenHash::of(&SessionToken::new("fictional-hourly-token".into()));
    let future = TokenHash::of(&SessionToken::new("fictional-future-token".into()));
    {
        let mut store = SqliteStore::open(database.path()).unwrap();
        for (hash, expiry) in [
            (expired.clone(), baseline.saturating_sub(1)),
            (due.clone(), baseline + 30_000),
            (future.clone(), baseline + 3 * SESSION_SWEEP_INTERVAL_MS),
        ] {
            store
                .insert_session(Session {
                    token_hash: hash,
                    user: UserId(10),
                    device: "Fictional desktop".into(),
                    created_at_unix_ms: baseline,
                    expires_at_unix_ms: expiry,
                })
                .unwrap();
        }
    }
    let state = stay_awake(AppState::open(database.path(), FAST))
        .await
        .unwrap();
    let read = |state: AppState, hash: TokenHash| async move {
        stay_awake(
            state
                .database
                .call(move |server| Ok(server.store().session(&hash)?)),
        )
        .await
        .unwrap()
    };
    assert!(read(state.clone(), expired).await.is_none());
    assert!(read(state.clone(), due.clone()).await.is_some());
    tokio::time::advance(Duration::from_millis(SESSION_SWEEP_INTERVAL_MS - 1)).await;
    tokio::task::yield_now().await;
    assert!(read(state.clone(), due.clone()).await.is_some());
    tokio::time::advance(Duration::from_millis(1)).await;
    // Let the ready maintenance task submit its delete before the read barrier.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    let mut removed = false;
    for _ in 0..100 {
        if read(state.clone(), due.clone()).await.is_none() {
            removed = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        removed,
        "the hourly maintenance task did not delete the expired row"
    );
    assert!(read(state.clone(), future).await.is_some());
    stay_awake(state.shutdown()).await.unwrap();
}
