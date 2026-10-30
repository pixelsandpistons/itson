# SQLite storage

`itson_server::sqlite::SqliteStore` implements the synchronous `Store` trait. Open a database file
in a host-chosen, existing directory and pass the store to `Server::open`:

```rust,ignore
let store = itson_server::sqlite::SqliteStore::open(data_dir.join("itson.sqlite3"))?;
let server = itson_server::Server::open(store, password_cost)?;
```

`open_in_memory` provides an isolated, temporary database for tests. The adapter performs blocking
I/O. The HTTP adapter's dedicated database worker owns `Server<SqliteStore>`; handlers await channel
replies rather than doing database work on async executor threads. The `itson-server` binary creates
`DIR/itson.sqlite3` from `--data-dir DIR`. See [GATEWAY.md](GATEWAY.md) for the worker, HTTP API and
connection lifetime design.

## Files and durability

File databases use SQLite's write-ahead log (`journal_mode=WAL`) with `synchronous=FULL`. SQLite
may create `itson.sqlite3-wal` and `itson.sqlite3-shm` beside the database. Keep the directory private
to the host: it contains messages, password hashes and session token hashes. Plain session tokens
are never written by the adapter.

The connection waits up to five seconds for database locks. Every store write is atomic. Inserts
that issue IDs update the high-water mark in the same transaction, and account role replacement is
transactional. This does not make a sequence of separate service operations one transaction.

Stop all users of the database before copying its files. A future online-backup command must use
SQLite's backup API; copying only the main file while WAL is active is not a consistent backup.

## Schema versions

`PRAGMA user_version` is the schema version, initially zero for a new database. On open, the adapter
takes an immediate transaction before reading that version, applies pending migrations in order,
updates the version, and commits. A migration failure rolls back the schema and version together.
Databases with newer versions are rejected with a backend error; there are no down migrations.

| Version | Migration | Contents |
|---|---|---|
| 1 | [001_initial.sql](../crates/itson-server/src/sqlite/migrations/001_initial.sql) | Settings, accounts and ordered roles, sessions, invites, roles, channels and ordered overwrites, messages, and the ID high-water mark. |
| 2 | [002_sessions_expiry_index.sql](../crates/itson-server/src/sqlite/migrations/002_sessions_expiry_index.sql) | Index on `sessions(expires_at)` for expiry cleanup; existing data is retained. |

Add future migration files to the ordered `MIGRATIONS` list in `sqlite.rs`; never rewrite an applied
migration. A fixture with an older schema should prove each upgrade preserves its data.

## Data representation

| Table | Purpose |
|---|---|
| `store_metadata` | Singleton with the largest successfully inserted account, role, channel or message ID, retained after deletion. |
| `settings` | Optional singleton with the name, owner, everyone permissions and registration policy. |
| `accounts`, `account_roles` | Unique, immutable usernames; credential hashes; moderation state; role IDs ordered by ordinal, including duplicates. |
| `sessions` | SHA-256 token hashes, user IDs, device names and expiry times; indexed by user and expiry. |
| `invites` | Invite codes, creator, times and use limits. |
| `roles` | Permission flags and display positions. |
| `channels`, `channel_overwrites` | Channel kinds, parents, positions and overwrites ordered by ordinal, including duplicate targets. |
| `messages` | Content, reply and edit fields; indexed by `(channel, id)` for exclusive cursor pagination, newest first. |

IDs and millisecond timestamps are **eight-byte, big-endian BLOBs**. The Rust boundary accepts `u64`,
while SQLite INTEGER is signed; this representation round-trips every accepted value without a lossy
cast. Fixed-width BLOB comparison preserves unsigned numeric ID ordering. Protocol IDs remain
53-bit numbers; storage does not change the wire format. Use `hex(id)` to inspect an ID, for example:

```sql
SELECT hex(id), content FROM messages ORDER BY id DESC LIMIT 20;
```

Permissions and other numeric fields fit in SQLite INTEGER; optional values use NULL. Strings use
TEXT with binary comparison, matching the case-sensitive equality in `MemoryStore`. Channel kinds
are `text`, `voice` and `category`; overwrite targets are `role` or `member`.

Foreign keys enforce only the internal ownership of the ordered child rows. References between
domain entities may be absent, just as in `MemoryStore`; service operations enforce domain rules.
Unique ID, username, invite and token-hash collisions return `StoreError::Conflict`. Missing updates
and deletes return `NotFound`; other SQLite failures return `Backend` for the host's log. The service
maps storage errors to its existing member-readable errors and protocol `ErrorCode`.

The shared conformance suite runs identical expectations against `MemoryStore` and `SqliteStore`.
File-backed tests cover reopen persistence, WAL, concurrent initialization, schema version checks
and rollback of a failed migration.

`Store::delete_expired_sessions(now)` removes only sessions whose expiry is at or before `now`
and returns the count. The adapter runs it on its database worker at startup and hourly. The cutoff
uses the same unsigned BLOB encoding as stored expiry times; account data and the ID high-water
mark are retained. Gateway logs remain subject to the core's periodic token revalidation and
disconnect grace period.
