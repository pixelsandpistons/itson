//! Exercise the adapter over real loopback HTTP and WebSocket connections.

use itson_protocol::frame::{ClientFrame, ErrorCode, ServerEnvelope, ServerFrame};
use itson_protocol::limits::MAX_FRAME_BYTES;
use itson_protocol::model::{ChannelKind, Message};
use itson_protocol::version::VersionRange;
use itson_protocol::{ChannelId, SessionToken};
use itson_server::Server;
use itson_server::http::{AppState, router};
use itson_server::password::Cost;
use itson_server::sqlite::SqliteStore;
use itson_server::store::{Store, StoredChannel};
use serde_json::{Value, json};
use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::protocol::frame::{
    Frame,
    coding::{Data, OpCode},
};
use tokio_tungstenite::tungstenite::{self, Message as WsMessage, WebSocket};

type TestError = Box<dyn Error + Send + Sync>;
type TestResult<T = ()> = Result<T, TestError>;
type Socket = WebSocket<TcpStream>;

const FAST: Cost = Cost { iterations: 1_000 };
const PASSWORD: &str = "fictional moon garden password";
const USERNAME: &str = "moon_moth";

#[derive(Debug)]
struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir()
                .join(format!("itson-http-tests-{}-{number}", std::process::id()));
            match fs::create_dir(&directory) {
                Ok(()) => return Ok(Self(directory)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other(
            "could not create a test database directory",
        ))
    }

    fn path(&self) -> PathBuf {
        self.0.join("fictional-server.sqlite3")
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn run_server(
    database: TestDatabase,
    scenario: impl FnOnce(SocketAddr) -> TestResult + Send + 'static,
) -> TestResult {
    let state = AppState::open(database.path(), FAST).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    // The synchronous client has bounded socket timeouts and runs off the
    // async executor. No extra HTTP client or futures extension crate is needed.
    let result = tokio::task::spawn_blocking(move || scenario(address)).await;
    server.abort();
    let _ = server.await;
    // Close the worker's database handle before the fixture directory is
    // removed, including when the client scenario failed or panicked.
    let shutdown = state.shutdown().await;
    result??;
    shutdown?;
    Ok(())
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Value,
}

fn http(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> TestResult<HttpResponse> {
    let payload = body
        .map(|value| serde_json::to_vec(&value))
        .transpose()?
        .unwrap_or_default();
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        payload.len()
    )?;
    if let Some(token) = token {
        write!(stream, "Authorization: Bearer {token}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.write_all(&payload)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let split = response
        .windows(4)
        .position(|v| v == b"\r\n\r\n")
        .ok_or("HTTP response has no header terminator")?;
    let head = std::str::from_utf8(&response[..split])?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or("HTTP response has no status")?
        .parse()?;
    let headers: Vec<_> = lines
        .map(|line| {
            let (name, value) = line.split_once(':').ok_or("invalid HTTP response header")?;
            Ok((name.to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect::<TestResult<_>>()?;
    let mut body = response[split + 4..].to_vec();
    if headers
        .iter()
        .any(|(name, value)| name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked"))
    {
        let mut decoded = Vec::new();
        let mut remaining = body.as_slice();
        loop {
            let end = remaining
                .windows(2)
                .position(|v| v == b"\r\n")
                .ok_or("invalid HTTP chunk header")?;
            let size = std::str::from_utf8(&remaining[..end])?
                .split(';')
                .next()
                .ok_or("missing chunk size")?;
            let size = usize::from_str_radix(size, 16)?;
            remaining = &remaining[end + 2..];
            if size == 0 {
                break;
            }
            let chunk = remaining.get(..size).ok_or("truncated HTTP chunk")?;
            decoded.extend_from_slice(chunk);
            remaining = remaining
                .get(size + 2..)
                .ok_or("truncated HTTP chunk terminator")?;
        }
        body = decoded;
    }
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)?
    };
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn setup(address: SocketAddr) -> TestResult<u64> {
    let response = http(
        address,
        "POST",
        "/api/setup",
        None,
        Some(json!({
            "server_name": "Fictional moon garden", "username": USERNAME, "password": PASSWORD
        })),
    )?;
    assert_eq!(response.status, 201, "{response:?}");
    Ok(response.body["user_id"]
        .as_u64()
        .ok_or("setup did not return user_id")?)
}

fn sign_in(address: SocketAddr, username: &str, device: &str) -> TestResult<String> {
    let response = http(
        address,
        "POST",
        "/api/sign-in",
        None,
        Some(json!({
            "username": username, "password": PASSWORD, "device": device
        })),
    )?;
    assert_eq!(response.status, 200, "{response:?}");
    assert_eq!(response.body["user"]["username"], username);
    assert!(response.body.get("password").is_none());
    assert!(
        response
            .headers
            .iter()
            .any(|(name, value)| name == "cache-control" && value == "no-store")
    );
    Ok(response.body["token"]
        .as_str()
        .ok_or("sign-in did not return token")?
        .to_owned())
}

fn socket(address: SocketAddr) -> TestResult<Socket> {
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let (socket, response) = tungstenite::client(format!("ws://{address}/gateway"), stream)?;
    assert_eq!(response.status().as_u16(), 101);
    Ok(socket)
}

fn send(socket: &mut Socket, frame: ClientFrame) -> TestResult {
    socket.send(WsMessage::Text(serde_json::to_string(&frame)?.into()))?;
    Ok(())
}

fn receive(socket: &mut Socket) -> TestResult<ServerEnvelope> {
    loop {
        match socket.read()? {
            WsMessage::Text(text) => return Ok(serde_json::from_str(text.as_str())?),
            WsMessage::Ping(_) => socket.flush()?,
            WsMessage::Pong(_) => {}
            other => return Err(format!("expected a server envelope, received {other:?}").into()),
        }
    }
}

fn hello(socket: &mut Socket) -> TestResult {
    send(
        socket,
        ClientFrame::Hello {
            client: "fictional-loopback-client".into(),
            versions: VersionRange::SUPPORTED,
        },
    )?;
    let welcome = receive(socket)?;
    assert!(welcome.s.is_none());
    assert!(matches!(welcome.frame, ServerFrame::Welcome { .. }));
    Ok(())
}

fn authenticate(socket: &mut Socket, token: &str) -> TestResult<ChannelId> {
    hello(socket)?;
    send(
        socket,
        ClientFrame::Authenticate {
            token: SessionToken::new(token.into()),
        },
    )?;
    let ready = receive(socket)?;
    let ServerFrame::Ready { channels, .. } = ready.frame else {
        return Err("expected ready".into());
    };
    Ok(channels
        .first()
        .ok_or("setup created no default channel")?
        .id)
}

fn post(
    socket: &mut Socket,
    channel: ChannelId,
    nonce: u64,
    content: &str,
) -> TestResult<ServerEnvelope> {
    send(
        socket,
        ClientFrame::SendMessage {
            channel,
            nonce,
            content: content.into(),
            reply_to: None,
        },
    )?;
    let envelope = receive(socket)?;
    assert!(envelope.s.is_some());
    assert!(
        matches!(&envelope.frame, ServerFrame::MessageCreated { message, nonce: Some(echo) }
        if message.content == content && *echo == nonce)
    );
    Ok(envelope)
}

fn created(envelope: &ServerEnvelope) -> &Message {
    match &envelope.frame {
        ServerFrame::MessageCreated { message, .. } => message,
        other => panic!("expected message_created, got {other:?}"),
    }
}

fn error(response: &HttpResponse, status: u16, code: &str) {
    assert_eq!(response.status, status, "{response:?}");
    assert_eq!(response.body["code"], code);
    assert!(
        response.body["detail"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_sign_in_post_timer_delivery_history_and_resume() -> TestResult {
    run_server(TestDatabase::new()?, |address| {
        let owner = setup(address)?;
        assert!(owner > 0);
        let first_token = sign_in(address, USERNAME, "Fictional first device")?;
        let second_token = sign_in(address, USERNAME, "Fictional second device")?;
        let mut first = socket(address)?;
        let channel = authenticate(&mut first, &first_token)?;
        let mut second = socket(address)?;
        assert_eq!(authenticate(&mut second, &second_token)?, channel);

        let initial = post(&mut first, channel, 71, "Fictional lunar message. 🌙")?;
        assert_eq!(created(&receive(&mut second)?), created(&initial));
        let delivered = post(
            &mut second,
            channel,
            72,
            "A fictional message for an idle socket.",
        )?;
        // No input is sent on first: this delivery requires the adapter timer.
        let last = receive(&mut first)?;
        assert_eq!(created(&last), created(&delivered));
        let last_seq = last.s.ok_or("message has no sequence")?;
        drop(first);

        let missed = post(
            &mut second,
            channel,
            73,
            "A fictional message during disconnection.",
        )?;
        let mut resumed = socket(address)?;
        hello(&mut resumed)?;
        send(
            &mut resumed,
            ClientFrame::Resume {
                token: SessionToken::new(first_token.clone()),
                last_seq,
            },
        )?;
        assert_eq!(
            receive(&mut resumed)?.frame,
            ServerFrame::Resumed { replayed: 1 }
        );
        let replay = receive(&mut resumed)?;
        assert_eq!(replay.s, Some(last_seq + 1));
        assert_eq!(replay.frame, missed.frame);
        send(&mut resumed, ClientFrame::Heartbeat { seq: 900 })?;
        assert_eq!(
            receive(&mut resumed)?.frame,
            ServerFrame::HeartbeatAck { seq: 900 }
        );

        let history = http(
            address,
            "GET",
            &format!("/api/history?channel={channel}&limit=2"),
            Some(&first_token),
            None,
        )?;
        assert_eq!(history.status, 200, "{history:?}");
        let history: Vec<Message> = serde_json::from_value(history.body)?;
        assert_eq!(
            history,
            vec![created(&missed).clone(), created(&delivered).clone()]
        );
        let history = http(
            address,
            "GET",
            &format!(
                "/api/history?channel={channel}&before={}&limit=1",
                created(&delivered).id
            ),
            Some(&first_token),
            None,
        )?;
        assert_eq!(history.status, 200);
        assert_eq!(
            serde_json::from_value::<Vec<Message>>(history.body)?,
            vec![created(&initial).clone()]
        );
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_rejections_have_codes_and_plain_text() -> TestResult {
    run_server(TestDatabase::new()?, |address| {
        setup(address)?;
        let repeated = http(address, "POST", "/api/setup", None, Some(json!({
            "server_name": "Fictional second garden", "username": "fern_sprite", "password": PASSWORD
        })))?;
        error(&repeated, 409, "invalid_content");
        let wrong_password = http(address, "POST", "/api/sign-in", None, Some(json!({
            "username": USERNAME, "password": "fictional wrong password", "device": "Fictional device"
        })))?;
        error(&wrong_password, 401, "invalid_content");
        let register = http(address, "POST", "/api/register", None, Some(json!({
            "username": "fern_sprite", "password": PASSWORD
        })))?;
        error(&register, 400, "invalid_content");
        let missing = http(address, "GET", "/api/history?channel=1", None, None)?;
        error(&missing, 401, "invalid_token");
        let invalid = http(address, "GET", "/api/history?channel=1", Some("fictional-invalid-token"), None)?;
        error(&invalid, 401, "invalid_token");
        let malformed = http(address, "POST", "/api/sign-in", None, Some(json!({"username": 42})))?;
        error(&malformed, 400, "malformed");
        let token = sign_in(address, USERNAME, "Fictional query device")?;
        let malformed = http(address, "GET", "/api/history?channel=fictional-not-a-number", Some(&token), None)?;
        error(&malformed, 400, "malformed");
        let missing_channel = http(address, "GET", "/api/history?channel=1", Some(&token), None)?;
        error(&missing_channel, 404, "not_found");
        let oversized = http(address, "POST", "/api/sign-in", None, Some(json!({
            "username": USERNAME, "password": "x".repeat(MAX_FRAME_BYTES), "device": "Fictional device"
        })))?;
        error(&oversized, 413, "frame_too_large");
        Ok(())
    }).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_uses_the_invite_and_signs_in_the_new_account() -> TestResult {
    let database = TestDatabase::new()?;
    let invite = {
        let store = SqliteStore::open(database.path())?;
        let mut server = Server::open(store, FAST)?;
        let now = itson_protocol::id::ITSON_EPOCH_MS + 1_000;
        let owner = server.setup("Fictional seeded garden", USERNAME, PASSWORD, now)?;
        server.create_invite(owner, None, Some(1), now)?.code
    };
    run_server(database, move |address| {
        let response = http(
            address,
            "POST",
            "/api/register",
            None,
            Some(json!({
                "invite_code": invite, "username": "fern_sprite", "password": PASSWORD
            })),
        )?;
        assert_eq!(response.status, 201, "{response:?}");
        assert!(response.body["user_id"].as_u64().is_some());
        let token = sign_in(address, "fern_sprite", "Fictional fern device")?;
        let mut connection = socket(address)?;
        authenticate(&mut connection, &token)?;
        let exhausted = http(
            address,
            "POST",
            "/api/register",
            None,
            Some(json!({
                "invite_code": invite, "username": "cloud_sprite", "password": PASSWORD
            })),
        )?;
        error(&exhausted, 400, "invalid_content");
        Ok(())
    })
    .await
}

fn gateway_error_then_close(socket: &mut Socket, code: ErrorCode) -> TestResult {
    let envelope = receive(socket)?;
    assert!(
        matches!(envelope.frame, ServerFrame::Error { code: actual, detail, .. }
        if actual == code && !detail.is_empty())
    );
    let WsMessage::Close(Some(frame)) = socket.read()? else {
        return Err("expected a WebSocket close frame".into());
    };
    let expected = match code {
        ErrorCode::FrameTooLarge => 1009,
        ErrorCode::Malformed => 1007,
        ErrorCode::Internal => 1011,
        _ => 1008,
    };
    assert_eq!(u16::from(frame.code), expected);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_rejects_bad_order_invalid_token_and_malformed_input() -> TestResult {
    run_server(TestDatabase::new()?, |address| {
        setup(address)?;
        let mut connection = socket(address)?;
        send(&mut connection, ClientFrame::Heartbeat { seq: 1 })?;
        gateway_error_then_close(&mut connection, ErrorCode::NotAuthenticated)?;
        let mut connection = socket(address)?;
        hello(&mut connection)?;
        send(
            &mut connection,
            ClientFrame::Authenticate {
                token: SessionToken::new("fictional-invalid-session".into()),
            },
        )?;
        gateway_error_then_close(&mut connection, ErrorCode::InvalidToken)?;
        let mut connection = socket(address)?;
        connection.send(WsMessage::Text("{".into()))?;
        gateway_error_then_close(&mut connection, ErrorCode::Malformed)?;
        let mut connection = socket(address)?;
        connection.send(WsMessage::Binary(b"fictional-binary-frame".to_vec().into()))?;
        gateway_error_then_close(&mut connection, ErrorCode::Malformed)?;
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_rejects_a_valid_json_message_over_the_frame_budget() -> TestResult {
    run_server(TestDatabase::new()?, |address| {
        let mut connection = socket(address)?;
        let oversized = ClientFrame::Hello {
            client: "x".repeat(MAX_FRAME_BYTES),
            versions: VersionRange::SUPPORTED,
        };
        send(&mut connection, oversized)?;
        oversized_rejected(&mut connection)?;
        let mut valid = socket(address)?;
        hello(&mut valid)?;
        Ok(())
    })
    .await
}

fn oversized_rejected(connection: &mut Socket) -> TestResult {
    loop {
        match connection.read() {
            Ok(WsMessage::Text(text)) => {
                let envelope: ServerEnvelope = serde_json::from_str(text.as_str())?;
                assert!(matches!(
                    envelope.frame,
                    ServerFrame::Error {
                        code: ErrorCode::FrameTooLarge,
                        ..
                    }
                ));
            }
            Ok(WsMessage::Close(_))
            | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed)
            | Err(tungstenite::Error::Protocol(
                tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
            )) => break,
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::BrokenPipe
                ) =>
            {
                break;
            }
            Err(error) => return Err(error.into()),
            other => return Err(format!("oversized message was not rejected: {other:?}").into()),
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_enforces_the_budget_across_text_fragments() -> TestResult {
    run_server(TestDatabase::new()?, |address| {
        let mut connection = socket(address)?;
        let payload = serde_json::to_vec(&ClientFrame::Hello {
            client: "x".repeat(MAX_FRAME_BYTES),
            versions: VersionRange::SUPPORTED,
        })?;
        let middle = payload.len() / 2;
        assert!(middle < MAX_FRAME_BYTES);
        assert!(payload.len() - middle < MAX_FRAME_BYTES);
        assert!(payload.len() > MAX_FRAME_BYTES);
        connection.send(WsMessage::Frame(Frame::message(
            payload[..middle].to_vec(),
            OpCode::Data(Data::Text),
            false,
        )))?;
        connection.send(WsMessage::Frame(Frame::message(
            payload[middle..].to_vec(),
            OpCode::Data(Data::Continue),
            true,
        )))?;
        oversized_rejected(&mut connection)?;
        let mut valid = socket(address)?;
        hello(&mut valid)?;
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_ready_is_replaced_with_a_size_error_and_close() -> TestResult {
    let database = TestDatabase::new()?;
    {
        let store = SqliteStore::open(database.path())?;
        let mut server = Server::open(store, FAST)?;
        server.setup(
            "Fictional crowded garden",
            USERNAME,
            PASSWORD,
            itson_protocol::id::ITSON_EPOCH_MS + 1_000,
        )?;
    }
    {
        let mut store = SqliteStore::open(database.path())?;
        for index in 0..1_024_u32 {
            store.insert_channel(StoredChannel {
                id: ChannelId(10_000 + u64::from(index)),
                name: format!("fictional-garden-{index:04}"),
                kind: ChannelKind::Text,
                parent: None,
                position: index,
                overwrites: Vec::new(),
            })?;
        }
    }
    run_server(database, |address| {
        let token = sign_in(address, USERNAME, "Fictional crowded device")?;
        let mut connection = socket(address)?;
        hello(&mut connection)?;
        send(
            &mut connection,
            ClientFrame::Authenticate {
                token: SessionToken::new(token),
            },
        )?;
        gateway_error_then_close(&mut connection, ErrorCode::FrameTooLarge)?;
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_resume_takes_ownership_without_competing_outbox_drains() -> TestResult {
    run_server(TestDatabase::new()?, |address| {
        setup(address)?;
        let token = sign_in(address, USERNAME, "Fictional resumed device")?;
        let mut original = socket(address)?;
        let channel = authenticate(&mut original, &token)?;
        let initial = post(
            &mut original,
            channel,
            800,
            "A fictional pre-resume message.",
        )?;
        let mut replacement = socket(address)?;
        hello(&mut replacement)?;
        send(
            &mut replacement,
            ClientFrame::Resume {
                token: SessionToken::new(token),
                last_seq: initial.s.ok_or("message has no sequence")?,
            },
        )?;
        assert_eq!(
            receive(&mut replacement)?.frame,
            ServerFrame::Resumed { replayed: 0 }
        );
        gateway_error_then_close(&mut original, ErrorCode::NotAuthenticated)?;
        let after = post(
            &mut replacement,
            channel,
            801,
            "A fictional post-resume message.",
        )?;
        assert_eq!(after.s, initial.s.map(|seq| seq + 1));
        send(&mut replacement, ClientFrame::Heartbeat { seq: 901 })?;
        assert_eq!(
            receive(&mut replacement)?.frame,
            ServerFrame::HeartbeatAck { seq: 901 }
        );
        Ok(())
    })
    .await
}
