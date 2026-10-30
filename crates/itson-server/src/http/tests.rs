#![allow(clippy::unwrap_used)]

use super::*;
use crate::store::MemoryStore;
use itson_protocol::model::Presence;
use itson_protocol::version::VersionRange;

const FICTIONAL_NOW: u64 = 1_800_000_000_000;
const FAST: Cost = Cost { iterations: 1_000 };

struct Fixture {
    server: Server<MemoryStore>,
    shared: Arc<Mutex<GatewayState>>,
    owner: UserId,
    owner_token: SessionToken,
    owner_hash: TokenHash,
    owner_lease: Arc<SocketLease>,
    observer_hash: TokenHash,
}

fn new_lease() -> Arc<SocketLease> {
    Arc::new(SocketLease {
        live: AtomicBool::new(true),
    })
}

fn hello() -> Connection {
    Connection::new("Fictional moon garden")
}

fn authenticate(
    server: &mut Server<MemoryStore>,
    gateway: &mut GatewayState,
    token: &SessionToken,
) -> Connection {
    let mut connection = hello();
    connection.handle(
        ClientFrame::Hello {
            client: "Fictional desktop".into(),
            versions: VersionRange::SUPPORTED,
        },
        server,
        &mut gateway.sessions,
        FICTIONAL_NOW,
    );
    let outputs = connection.handle(
        ClientFrame::Authenticate {
            token: token.clone(),
        },
        server,
        &mut gateway.sessions,
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

fn fixture() -> Fixture {
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
            "Fictional observer desktop",
            FICTIONAL_NOW,
        )
        .unwrap();
    let owner_hash = TokenHash::of(&owner_token);
    let observer_hash = TokenHash::of(&observer_token);
    let owner_lease = new_lease();
    let mut gateway = GatewayState::default();
    authenticate(&mut server, &mut gateway, &owner_token);
    authenticate(&mut server, &mut gateway, &observer_token);
    gateway
        .owners
        .insert(owner_hash.clone(), Arc::clone(&owner_lease));
    gateway.owners.insert(observer_hash.clone(), new_lease());
    gateway.sessions.drain(&owner_hash);
    gateway.sessions.drain(&observer_hash);
    Fixture {
        server,
        shared: Arc::new(Mutex::new(gateway)),
        owner,
        owner_token,
        owner_hash,
        owner_lease,
        observer_hash,
    }
}

fn disconnect(shared: &Arc<Mutex<GatewayState>>, lease: &Arc<SocketLease>) -> Disconnect {
    Disconnect {
        gateway: Arc::clone(shared),
        lease: Arc::clone(lease),
    }
}

async fn advance(duration: Duration) {
    tokio::time::advance(duration).await;
    tokio::task::yield_now().await;
}

fn assert_offline_event(envelopes: &[ServerEnvelope], user: UserId) {
    assert_eq!(envelopes.len(), 1);
    assert!(envelopes[0].s.is_some());
    assert_eq!(
        envelopes[0].frame,
        ServerFrame::PresenceUpdated {
            user,
            presence: Presence::Offline,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn disconnect_keeps_the_log_until_sixty_seconds_then_publishes_offline() {
    let fixture = fixture();
    drop(disconnect(&fixture.shared, &fixture.owner_lease));
    assert!(!fixture.owner_lease.live.load(Ordering::Acquire));
    // Let the spawned cleanup task establish its deadline before moving time.
    tokio::task::yield_now().await;
    advance(Duration::from_secs(59)).await;
    {
        let mut gateway = fixture.shared.lock().await;
        assert_eq!(gateway.sessions.len(), 2);
        assert_eq!(
            gateway.sessions.presence_of(fixture.owner),
            Presence::Online
        );
        assert!(gateway.owners.contains_key(&fixture.owner_hash));
        assert!(gateway.sessions.drain(&fixture.observer_hash).is_empty());
    }
    advance(Duration::from_secs(1)).await;
    {
        let mut gateway = fixture.shared.lock().await;
        assert_eq!(gateway.sessions.len(), 1);
        assert_eq!(
            gateway.sessions.presence_of(fixture.owner),
            Presence::Offline
        );
        assert!(!gateway.owners.contains_key(&fixture.owner_hash));
        assert_offline_event(
            &gateway.sessions.drain(&fixture.observer_hash),
            fixture.owner,
        );
    }
    advance(Duration::from_secs(1)).await;
    assert!(
        fixture
            .shared
            .lock()
            .await
            .sessions
            .drain(&fixture.observer_hash)
            .is_empty()
    );
}

#[tokio::test(start_paused = true)]
async fn only_the_last_session_to_expire_takes_its_member_offline() {
    let mut fixture = fixture();
    let (second_token, _) = fixture
        .server
        .sign_in(
            "moon_moth",
            "fictional garden password",
            "Fictional second desktop",
            FICTIONAL_NOW,
        )
        .unwrap();
    let second_hash = TokenHash::of(&second_token);
    let second_lease = new_lease();
    {
        let mut gateway = fixture.shared.lock().await;
        authenticate(&mut fixture.server, &mut gateway, &second_token);
        gateway
            .owners
            .insert(second_hash.clone(), Arc::clone(&second_lease));
    }
    drop(disconnect(&fixture.shared, &fixture.owner_lease));
    tokio::task::yield_now().await;
    advance(RESUME_GRACE).await;
    {
        let mut gateway = fixture.shared.lock().await;
        assert_eq!(gateway.sessions.len(), 2);
        assert_eq!(
            gateway.sessions.presence_of(fixture.owner),
            Presence::Online
        );
        assert!(gateway.owners.contains_key(&second_hash));
        assert!(gateway.sessions.drain(&fixture.observer_hash).is_empty());
    }
    drop(disconnect(&fixture.shared, &second_lease));
    tokio::task::yield_now().await;
    advance(RESUME_GRACE).await;
    let mut gateway = fixture.shared.lock().await;
    assert_eq!(gateway.sessions.len(), 1);
    assert_eq!(
        gateway.sessions.presence_of(fixture.owner),
        Presence::Offline
    );
    assert_offline_event(
        &gateway.sessions.drain(&fixture.observer_hash),
        fixture.owner,
    );
}

#[tokio::test(start_paused = true)]
async fn an_old_disconnect_timer_cannot_close_a_resumed_session() {
    let mut fixture = fixture();
    drop(disconnect(&fixture.shared, &fixture.owner_lease));
    tokio::task::yield_now().await;
    advance(Duration::from_secs(30)).await;
    let resumed_lease = new_lease();
    {
        let mut gateway = fixture.shared.lock().await;
        let mut connection = hello();
        connection.handle(
            ClientFrame::Hello {
                client: "Fictional reconnected desktop".into(),
                versions: VersionRange::SUPPORTED,
            },
            &mut fixture.server,
            &mut gateway.sessions,
            FICTIONAL_NOW,
        );
        let outputs = connection.handle(
            ClientFrame::Resume {
                token: fixture.owner_token.clone(),
                last_seq: 0,
            },
            &mut fixture.server,
            &mut gateway.sessions,
            FICTIONAL_NOW,
        );
        assert!(matches!(
            outputs.first(),
            Some(Output::Send(ServerEnvelope {
                frame: ServerFrame::Resumed { .. },
                ..
            }))
        ));
        gateway
            .owners
            .insert(fixture.owner_hash.clone(), Arc::clone(&resumed_lease));
    }
    advance(Duration::from_secs(31)).await;
    let mut gateway = fixture.shared.lock().await;
    assert_eq!(gateway.sessions.len(), 2);
    assert!(gateway.owns(&fixture.owner_hash, &resumed_lease));
    assert!(!gateway.owns(&fixture.owner_hash, &fixture.owner_lease));
    assert_eq!(
        gateway.sessions.presence_of(fixture.owner),
        Presence::Online
    );
    assert!(gateway.sessions.drain(&fixture.observer_hash).is_empty());
}

#[tokio::test(start_paused = true)]
async fn disconnecting_an_unauthenticated_socket_keeps_other_sessions() {
    let fixture = fixture();
    let unauthenticated = new_lease();
    drop(disconnect(&fixture.shared, &unauthenticated));
    tokio::task::yield_now().await;
    advance(RESUME_GRACE).await;
    let mut gateway = fixture.shared.lock().await;
    assert_eq!(gateway.sessions.len(), 2);
    assert!(gateway.owns(&fixture.owner_hash, &fixture.owner_lease));
    assert!(gateway.sessions.drain(&fixture.observer_hash).is_empty());
}

#[tokio::test(start_paused = true)]
async fn cleanup_finds_a_session_claimed_after_its_socket_was_dropped() {
    let fixture = fixture();
    fixture
        .shared
        .lock()
        .await
        .owners
        .remove(&fixture.owner_hash);
    drop(disconnect(&fixture.shared, &fixture.owner_lease));
    tokio::task::yield_now().await;
    // Model cancellation between a worker's authentication and its registry
    // claim: expiry must examine ownership later rather than snapshot it now.
    advance(Duration::from_secs(1)).await;
    fixture
        .shared
        .lock()
        .await
        .owners
        .insert(fixture.owner_hash.clone(), Arc::clone(&fixture.owner_lease));
    advance(Duration::from_secs(59)).await;
    let mut gateway = fixture.shared.lock().await;
    assert_eq!(gateway.sessions.len(), 1);
    assert!(!gateway.owners.contains_key(&fixture.owner_hash));
    assert_offline_event(
        &gateway.sessions.drain(&fixture.observer_hash),
        fixture.owner,
    );
}
