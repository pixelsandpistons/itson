# HTTP and WebSocket adapter

The `itson-server` binary opens a SQLite database and serves the account, history and gateway
endpoints through axum. Start it with an explicit data directory and listening address:

```sh
cargo run -p itson-server -- --data-dir ./fictional-server-data --listen 127.0.0.1:8080
```

The binary creates the directory and `itson.sqlite3` inside it. New directories use owner-only
permissions on Unix; choose a private location when using an existing directory or inherited
Windows permissions. Startup output gives the actual listening address and says whether setup has
run. HTTP and WebSocket traffic is plaintext in this version. **Use TLS or a reverse proxy that
provides TLS before exposing the server.**

## Initial setup code

An unconfigured server generates a setup code at startup. The binary prints it once as
`Setup code: <code>`. Send that code in the `setup_code` field of `POST /api/setup`, together with
the server name, username and password. Missing or incorrect codes return `403 forbidden` and
count toward the account-creation address limit.

`AppState::take_setup_code()` transfers the plaintext code to its caller once; the adapter then
retains only its hash for checking. There is no endpoint that reveals the code, and it is not saved
in SQLite. Successful setup clears the hash and refuses subsequent setup requests. Restarting an
unconfigured server generates a new code; restarting a configured server prints no code. Keep
startup output private to the person creating the owner account, and send the code in the JSON
body rather than a URL.

## HTTP endpoints

Protocol 0.3 marks this hardening change. Existing gateway envelope shapes are retained and older
gateway versions can still negotiate. HTTP setup now requires `setup_code`; an older setup client
must collect the host's printed code and include that field.

Requests and responses use JSON. A successful setup creates the owner account and the default
`general` text channel; it does not sign the owner in. Registration follows the service's existing
policy: it needs an invite unless the host has opened registration. The adapter does not add an
invite-management endpoint.

| Request | Input | Success response |
|---|---|---|
| `POST /api/setup` | `setup_code`, `server_name`, `username`, `password` | `201` with `{"user_id": <id>}`; refused after setup has run. |
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
| `403` | The account is banned or lacks permission, or the initial setup code is missing or wrong. |
| `404` | The requested resource was not found. |
| `409` | Setup already ran, the server needs setup, or the username is taken. |
| `413` | The request body is too large (`frame_too_large`). |
| `429` | A request bucket is exhausted or all password-verification slots are busy (`rate_limited`); includes `Retry-After`. |
| `500` | An internal or storage failure; details stay out of member-facing responses. |

## Rate limits and peer addresses

The adapter uses the core's dependency-free `RateLimiter` token buckets:

| Operation | Key | Burst | Refill |
|---|---|---|---|
| Setup and registration, sharing one bucket | Peer IP address | 5 attempts | 1 attempt per 60 seconds |
| Sign-in | Peer IP address and normalized username, checked separately | 10 attempts for each key | 1 attempt per 30 seconds |
| Gateway input | Socket connection | 60 frames | 1 frame per 100 milliseconds |

Setup attempts with missing or wrong codes still spend address tokens. The sign-in username key
uses the same normalization as account lookup; invalid usernames share one bucket rather than
retaining arbitrary input. Gateway accounting includes `hello`, `authenticate`, `resume`, and
WebSocket ping/pong input as well as subsequent JSON frames. Exceeding the gateway limit sends a
`rate_limited` error and closes the socket. HTTP `Retry-After` values round the remaining wait up
to whole seconds, with a minimum of one second.

Each HTTP limiter retains at most 4,096 keys. Maintenance sweeps fully refilled, inactive buckets
every three minutes; the core's cap evicts the stalest key when a new key would exceed the bound.

HTTP handlers use the IPv4 or IPv6 peer IP from axum's `ConnectInfo<SocketAddr>`, excluding the
source port. The binary serves the router with
`into_make_service_with_connect_info::<SocketAddr>()`; other callers must do the same. The adapter
does not trust `Forwarded` or `X-Forwarded-For`. Clients behind a reverse proxy therefore share
that proxy's address bucket. Explicit trusted-proxy configuration is a future task.

## Database worker and sign-in verification

`http::AppState::open(path, password_cost)` starts one dedicated operating-system thread. That
thread opens `SqliteStore`, constructs `Server<SqliteStore>`, and owns both for their lifetime.
Handlers submit closures through a bounded Tokio channel of 64 requests and await their individual
oneshot replies. SQLite work runs on that thread instead of Tokio's executor.

Sign-in uses three steps. The worker prepares an opaque credential challenge; `spawn_blocking`
verifies the supplied password and prepares any rehash; the worker finishes with an opaque,
single-use proof bound to the same server and credential snapshot. Before issuing a token, it
re-reads the account, confirms that the stored credential still matches, and checks the ban state.
This avoids treating a password checked against stale credentials as authorization to sign in.
Unknown or invalid usernames use one verification against a valid current-cost dummy hash created
when the server opens, rather than generating a new hash for each attempt. This describes the
password work performed; it is not a claim that request durations are equal.

At most four sign-in verification jobs are admitted at once. A permit stays inside the blocking
closure until its work ends, even if the HTTP caller disconnects. If no permit is available, the
adapter returns `429 rate_limited` with `Retry-After: 1`. Password hashing for setup and registration
continues to run on the database worker.

The adapter caches the server name and setup status at startup and updates them only after
successful setup. `AppState::setup_complete()` reads that cached status; gateway upgrades use the
cached name without a database lookup. `http::router(state)` builds the axum router.

After stopping the listener, `AppState::shutdown()` stops maintenance, stops accepting new database
work, lets queued operations finish, and waits for the worker to close the database. It is called
once; all state clones then reject new database requests. Maintenance holds weak references to its
database and limiter state, so it does not keep a dropped application alive; dropping its owner or
explicit shutdown aborts the task.

Gateway state consists of `Sessions` and a registry of socket owners behind a Tokio mutex. The
database worker uses `blocking_lock()` while calling synchronous connection operations; async
draining and expiry tasks use `lock().await`. No mutex guard is held while sending to a socket.
This avoids requiring SQLite to be `Sync`. Each individual store write remains atomic as described
in [STORAGE.md](STORAGE.md); separate service calls do not become one database transaction.

## WebSocket delivery and housekeeping

Clients send JSON `ClientFrame` values, starting with `hello`, then `authenticate` or `resume`.
The adapter calls `Connection::handle` on the database worker and sends each `Output::Send` as a
JSON `ServerEnvelope`. An `Output::Close` ends the socket after its error explanation. The adapter
drains the connection's session outbox after processing input and when notified of pending work.

Each socket owner has a Tokio `Notify`. After a gateway operation, `wake_pending` checks the
core's read-only `Sessions::needs_drain` predicate and wakes the current owners whose outboxes
contain events or an overflow flag. Notifications also cover offline events published during
session cleanup. Taking ownership wakes the superseded socket so it can close; it no longer
competes to drain that session. The core exposes only the predicate and keeps no Tokio dependency.
Notification permits coalesce wakeups, so a quiet client can receive events without a 25 ms poll.

A separate timer calls `Connection::tick` through the worker about once a second. Notifications
also run housekeeping before draining. The core enforces these deadlines:

| State or check | Policy |
|---|---|
| Before authentication | Close after more than 10 seconds without a client frame; receiving `hello` resets that idle timer. |
| Ready connection | Close after more than 75 seconds without a client frame, including heartbeats. |
| Session validity | Re-check the stored session and account at 60-second intervals while ready; revoked, expired or banned sessions close. |
| Slow reader | A pending-outbox overflow closes the socket with `too_slow`; replay remains subject to its retained sequence window. |

The adapter anchors Unix time at startup and derives elapsed time from Tokio's monotonic clock.
Timer scheduling, queued worker work and socket writes can delay when a close is delivered; the
deadline values are not end-to-end latency guarantees. Each write and each complete output batch
has a five-second send deadline, so a reader making tiny progress cannot defer housekeeping by
holding one batch open indefinitely.

Both WebSocket frame size and assembled message size are limited to `MAX_FRAME_BYTES` (64 KiB),
including fragmented messages. Serialized outgoing envelopes are checked against the same limit.
Binary input is not part of this text gateway. A `ready` envelope that exceeds the limit is refused
with `frame_too_large`; there is no ad hoc splitting of the protocol frame.

## Resume and cleanup

Only one socket owns a given session token at a time. A successful new authentication or resume
takes ownership; the previous socket can no longer handle client frames or drain that session.
Different device tokens continue to have separate event logs, even for the same member.

When a socket ends, its drop guard marks it disconnected and schedules cleanup after 60 seconds.
The event log remains available during that period. Resume sends `resumed` followed by missed
events with their original sequence numbers. Cleanup compares socket-owner identity before
calling `Sessions::close`, so an old socket's timer cannot remove a session that has since resumed
on another socket. If closing the session returns a member whose last gateway session is gone,
the adapter calls `Sessions::publish_offline` and wakes the remaining recipients.

Draining the pending outbox does not erase the replay log. If a write to the socket fails after a
drain, a reconnecting client can still request replay from its last received sequence, subject to
the core's existing 512-event replay capacity. The pending outbox is also bounded to 512 events;
overflow signals a slow reader instead of continuing to grow. Process restart discards gateway
logs; a persisted sign-in token can authenticate a fresh connection until it expires or is revoked.

Expired persistent sessions are deleted on the database worker at startup and hourly thereafter.
`Store::delete_expired_sessions(now)` removes rows whose expiry is less than or equal to `now` and
returns the number removed. Migration
[002_sessions_expiry_index.sql](../crates/itson-server/src/sqlite/migrations/002_sessions_expiry_index.sql)
adds the SQLite expiry index. An hourly sweep is separate from live-connection revalidation and
the 60-second gateway resume grace period; expiry is enforced even before a row is swept.

## Remaining limits

Large `ready` payloads need a future protocol design for paging or chunking before they can exceed
the frame limit. Query batching for account roles and channel overwrites remains a storage
follow-up. Trusted-proxy configuration is not available. TLS, QUIC voice, Argon2, compression,
reactions and read markers are outside this hardening change.
