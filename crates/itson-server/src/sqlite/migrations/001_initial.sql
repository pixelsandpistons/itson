-- u64 values use eight-byte big-endian blobs (see docs/STORAGE.md).
CREATE TABLE store_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    max_id BLOB NOT NULL CHECK (length(max_id) = 8)
);
INSERT INTO store_metadata VALUES (1, X'0000000000000000');

CREATE TABLE settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    name TEXT NOT NULL,
    owner BLOB NOT NULL,
    everyone INTEGER NOT NULL,
    open_registration INTEGER NOT NULL
);
CREATE TABLE accounts (
    id BLOB PRIMARY KEY NOT NULL,
    username TEXT NOT NULL UNIQUE,
    display_name TEXT,
    password_hash TEXT NOT NULL,
    created_at BLOB NOT NULL,
    timed_out_until BLOB,
    banned INTEGER NOT NULL
);
CREATE TABLE account_roles (
    account BLOB NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    role BLOB NOT NULL,
    PRIMARY KEY (account, ordinal)
);
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY NOT NULL,
    user BLOB NOT NULL,
    device TEXT NOT NULL,
    created_at BLOB NOT NULL,
    expires_at BLOB NOT NULL
);
CREATE INDEX sessions_by_user ON sessions(user);
CREATE TABLE invites (
    code TEXT PRIMARY KEY NOT NULL,
    created_by BLOB NOT NULL,
    created_at BLOB NOT NULL,
    expires_at BLOB,
    max_uses INTEGER,
    uses INTEGER NOT NULL
);
CREATE TABLE roles (
    id BLOB PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    permissions INTEGER NOT NULL,
    position INTEGER NOT NULL
);
CREATE TABLE channels (
    id BLOB PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('text', 'voice', 'category')),
    parent BLOB,
    position INTEGER NOT NULL
);
CREATE TABLE channel_overwrites (
    channel BLOB NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    target_kind TEXT NOT NULL CHECK (target_kind IN ('role', 'member')),
    target BLOB NOT NULL,
    allow INTEGER NOT NULL,
    deny INTEGER NOT NULL,
    PRIMARY KEY (channel, ordinal)
);
CREATE TABLE messages (
    id BLOB PRIMARY KEY NOT NULL,
    channel BLOB NOT NULL,
    author BLOB NOT NULL,
    content TEXT NOT NULL,
    reply_to BLOB,
    edited_at BLOB
);
CREATE INDEX messages_by_channel_id ON messages(channel, id);
