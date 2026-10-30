# HTTP and WebSocket adapter

The `itson-server` binary opens a SQLite database and serves the account, history and gateway
endpoints through axum. Start it with an explicit data directory and listening address:

```sh
cargo run -p itson-server -- --data-dir ./fictional-server-data --listen 127.0.0.1:8080
```

The binary creates the directory and `itson.sqlite3` inside it. New directories use owner-only
permissions on Unix; choose a private location when using an existing directory or inherited
Windows permissions. Startup output gives the actual
listening address and says whether setup has run. HTTP and WebSocket traffic is plaintext in this
version. **Use TLS or a reverse proxy that provides TLS before exposing the server.** Keep initial
setup reachable only by the person creating the owner account.

## HTTP endpoints

Requests and responses use JSON. A successful setup creates the owner account and the default
`general` text channel; it does not sign the owner in. Registration follows the service's existing
policy: it needs an invite unless the host has opened registration. The adapter does not add an
invite-management endpoint.

| Request | Input | Success response |
|---|---|---|
| `POST /api/setup` | `server_name`, `username`, `password` | `201` with `{"user_id": <id>}`; refused after setup has run. |
| `POST /api/register` | `username`, `password`, optional `invite_code` | `201` with `{"user_id": <id>}`. |
| `POST /api/sign-in` | `username`, `password`, `device` | `200` with `{"token": <session token>, "user": <User>}`. |
| `GET /api/history` | Query `channel`, optional `before` and `limit`; `Authorization: Bearer <session token>` | `200` with an array of `Message` records, newest first. |
| `GET /gateway` | WebSocket upgrade | The gateway exchange described below. |

The sign-in response gives the session token to the client once. The database stores its SHA-256
hash. Subsequent authenticated HTTP requests send the token in the bearer header. Passwords and
tokens must not be placed in URLs. Responses carry `Cache-Control: no-store`.

History's `before` cursor is exclusive: only messages with smaller IDs are returned. The default
limit is 50; `Server::history` clamps it to 1 through 100 and checks the member's permissions.

Errors have the JSON shape `{"code": <ErrorCode>, "detail": <plain-language text>}`. Service errors
retain their existing `ErrorCode`; HTTP status describes how the request failed:

| HTTP status | Meaning |
|---|---|
| `400` | Invalid input or malformed JSON/query data. |
| `401` | Invalid credentials or missing, expired or invalid bearer token. |
| `403` | The account is banned or lacks permission. |
| `404` | The requested resource was not found. |
| `409` | Setup already ran, the server needs setup, or the username is taken. |
| `413` | The request body is too large (`frame_too_large`). |
| `500` | An internal or storage failure; details stay out of member-facing responses. |

## Database worker and shared sessions

`http::AppState::open(path, password_cost)` starts one dedicated operating-system thread. That
thread opens `SqliteStore`, constructs `Server<SqliteStore>`, and owns both for their lifetime.
Handlers submit closures through a bounded Tokio channel of 64 requests and await their individual
oneshot replies. SQLite work and password hashing run on that thread instead of Tokio's executor.
`AppState::setup_complete()` queries setup status through the same worker, and `http::router(state)`
builds the axum router.

After stopping the listener, `AppState::shutdown()` stops accepting new database work, lets queued
operations finish, and waits for the worker to close the database. It is called once; all state
clones then reject new database requests.

Gateway state consists of `Sessions` and a registry of socket owners behind a Tokio mutex. The
database worker uses `blocking_lock()` while calling the synchronous `Connection::handle`; async
draining and expiry tasks use `lock().await`. No mutex guard is held while sending to a socket.
This keeps the existing core and `Store` interfaces intact and avoids requiring SQLite to be
`Sync`. Each individual store write remains atomic as described in [STORAGE.md](STORAGE.md);
separate service calls do not become one database transaction.

## WebSocket exchange and resume

Clients send JSON `ClientFrame` values, starting with `hello`, then `authenticate` or `resume`.
The adapter calls `Connection::handle` on the database worker and sends each `Output::Send` as a
JSON `ServerEnvelope`. An `Output::Close` ends the socket after its error explanation. The adapter
drains the connection's session outbox after each received frame and on a timer scheduled every
25 milliseconds, so a quiet client can receive events from other members.

Both WebSocket frame size and assembled message size are limited to `MAX_FRAME_BYTES` (64 KiB),
including fragmented messages. Serialized outgoing envelopes are checked against the same limit.
Binary input is not part of this text gateway. A `ready` envelope that exceeds the limit is refused
with `frame_too_large`; there is no ad hoc splitting of the protocol frame.

Only one socket owns a given session token at a time. A successful new authentication or resume
takes ownership; the previous socket can no longer handle client frames or drain that session.
Different device tokens continue to have separate event logs, even for the same member.

When a socket ends, its drop guard marks it disconnected and schedules cleanup after 60 seconds.
The event log remains available during that period. Resume sends `resumed` followed by missed
events with their original sequence numbers. Cleanup compares socket-owner identity before
calling `Sessions::close`, so an old socket's timer cannot remove a session that has since resumed
on another socket. If closing the session returns a member whose last gateway session is gone,
the adapter calls `Sessions::publish_offline`.

Draining the pending outbox does not erase the replay log. If a write to the socket fails after a
drain, a reconnecting client can still request replay from its last received sequence, subject to
the core's existing 512-event replay capacity. Process restart discards gateway logs; the persisted
sign-in token remains subject to its normal expiry and can authenticate a fresh gateway connection.

## Remaining limits

The adapter leaves service and gateway policy in the core. The core currently acknowledges
heartbeats without expiring quiet connections, and it does not revalidate the sign-in token on
each frame after a gateway connection authenticates. Its replay log is bounded, but the pending
outbox has no separate size bound. Large `ready` payloads need a future protocol design for paging
or chunking before they can exceed the frame limit.

Expired SQLite sessions are not deleted by a scheduled cleanup task yet. Query batching for
account roles and channel overwrites remains a storage follow-up. TLS, QUIC voice, Argon2,
compression, reactions and read markers are outside this adapter change.
