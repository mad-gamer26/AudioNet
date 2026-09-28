//! SQLite storage: users, web sign-in sessions, devices (nodes) and email
//! links (address confirmation, password reset).
//!
//! Secrets are never stored directly: passwords are argon2id hashes and all
//! tokens are stored as SHA-256 hashes.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, params};

use crate::auth::TokenHash;

const SCHEMA_VERSION: i64 = 3;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Db")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    /// The confirmed address: where password-reset links go. Optional
    /// (older accounts, and new ones until they confirm).
    pub email: Option<String>,
    /// An address waiting for confirmation (the one given at sign-up, or a
    /// new one replacing `email`), until it is confirmed or expires. It does
    /// not keep anyone else from using the address.
    pub pending_email: Option<String>,
}

/// What an emailed link is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailPurpose {
    /// Confirms the address it was sent to.
    Verify,
    /// Sets a new password.
    Reset,
}

impl EmailPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Verify => "verify",
            Self::Reset => "reset",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeRow {
    pub id: String,
    pub user_id: i64,
    pub name: String,
    pub platform: Option<String>,
    pub last_seen: Option<i64>,
}

pub fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(
                "CREATE TABLE users (
                    id INTEGER PRIMARY KEY,
                    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    password_hash TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE web_sessions (
                    token_hash BLOB PRIMARY KEY,
                    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                    created_at INTEGER NOT NULL,
                    expires_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id TEXT PRIMARY KEY,
                    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                    name TEXT NOT NULL,
                    platform TEXT,
                    token_hash BLOB NOT NULL UNIQUE,
                    created_at INTEGER NOT NULL,
                    last_seen INTEGER
                );",
            )?;
        }
        if version < 2 {
            // Version 2: devices sign in with the account password; pairing
            // codes no longer exist.
            conn.execute_batch("DROP TABLE IF EXISTS pairing_codes;")?;
        }
        if version < 3 {
            // Version 3: email addresses (required for new accounts, optional
            // for older ones): a confirmed one, unique, and one waiting for
            // confirmation, which expires (NULL: never, on a server that
            // cannot send email); and single-use emailed links, stored
            // hashed.
            conn.execute_batch(
                "ALTER TABLE users ADD COLUMN email TEXT;
                ALTER TABLE users ADD COLUMN pending_email TEXT;
                ALTER TABLE users ADD COLUMN pending_email_expires_at INTEGER;
                CREATE UNIQUE INDEX users_email ON users (email COLLATE NOCASE);
                CREATE TABLE email_tokens (
                    token_hash BLOB PRIMARY KEY,
                    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                    purpose TEXT NOT NULL,
                    email TEXT NOT NULL,
                    expires_at INTEGER NOT NULL
                );",
            )?;
        }
        if version < SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `f` on a blocking thread with the connection.
    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> rusqlite::Result<T> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().unwrap_or_else(|e| e.into_inner());
            f(&c)
        })
        .await
        .expect("database task panicked")
    }

    /// Synchronous access, for the command-line tools.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        let c = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        f(&c)
    }
}

// ─── queries (all synchronous; wrap with Db::call) ─────────────────────────

pub fn create_user(c: &Connection, username: &str, password_hash: &str) -> rusqlite::Result<i64> {
    c.execute(
        "INSERT INTO users (username, password_hash, created_at) VALUES (?1, ?2, ?3)",
        params![username, password_hash, now_s()],
    )?;
    Ok(c.last_insert_rowid())
}

/// A waiting address counts only until it expires.
const USER_COLUMNS: &str = "id, username, password_hash, email,
    CASE WHEN pending_email_expires_at IS NULL OR pending_email_expires_at > unixepoch()
         THEN pending_email END";

fn user_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        username: r.get(1)?,
        password_hash: r.get(2)?,
        email: r.get(3)?,
        pending_email: r.get(4)?,
    })
}

pub fn user_by_name(c: &Connection, username: &str) -> rusqlite::Result<Option<User>> {
    c.query_row(
        &format!("SELECT {USER_COLUMNS} FROM users WHERE username = ?1"),
        params![username],
        user_row,
    )
    .optional()
}

pub fn user_by_id(c: &Connection, id: i64) -> rusqlite::Result<Option<User>> {
    c.query_row(
        &format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"),
        params![id],
        user_row,
    )
    .optional()
}

/// The account whose confirmed address is `email`.
pub fn user_by_email(c: &Connection, email: &str) -> rusqlite::Result<Option<User>> {
    c.query_row(
        &format!("SELECT {USER_COLUMNS} FROM users WHERE email = ?1 COLLATE NOCASE"),
        params![email],
        user_row,
    )
    .optional()
}

/// Whether another account has confirmed `email` (addresses waiting for
/// confirmation do not count).
pub fn email_taken(c: &Connection, email: &str, except_user: i64) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM users WHERE email = ?1 COLLATE NOCASE AND id != ?2)",
        params![email, except_user],
        |r| r.get(0),
    )
}

/// Sets (or, with `None`, removes) a user's confirmed address, dropping
/// any waiting one and outstanding links. Other accounts waiting for the
/// same address lose it: it is now confirmed as someone else's.
pub fn set_email(c: &Connection, user_id: i64, email: Option<&str>) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE users SET email = ?1, pending_email = NULL, pending_email_expires_at = NULL WHERE id = ?2",
        params![email, user_id],
    )?;
    c.execute(
        "DELETE FROM email_tokens WHERE user_id = ?1",
        params![user_id],
    )?;
    if let Some(e) = email {
        c.execute(
            "UPDATE users SET pending_email = NULL, pending_email_expires_at = NULL
             WHERE pending_email = ?1 COLLATE NOCASE AND id != ?2",
            params![e, user_id],
        )?;
    }
    Ok(())
}

/// Sets (or, with `None`, cancels) the address waiting for confirmation;
/// the confirmed one stays in use meanwhile. Earlier confirmation links stop
/// working. `expires_at`: when it is dropped unless confirmed (`None`:
/// never).
pub fn set_pending_email(
    c: &Connection,
    user_id: i64,
    email: Option<&str>,
    expires_at: Option<i64>,
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE users SET pending_email = ?1, pending_email_expires_at = ?2 WHERE id = ?3",
        params![email, email.and(expires_at), user_id],
    )?;
    c.execute(
        "DELETE FROM email_tokens WHERE user_id = ?1 AND purpose = 'verify'",
        params![user_id],
    )?;
    Ok(())
}

/// Gives the waiting address a new deadline (a new link was sent).
pub fn extend_pending_email(c: &Connection, user_id: i64, expires_at: i64) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE users SET pending_email_expires_at = ?1 WHERE id = ?2 AND pending_email IS NOT NULL",
        params![expires_at, user_id],
    )?;
    Ok(())
}

/// Makes the waiting address the confirmed one. `false` if another account
/// confirmed it first (it stays unconfirmed here).
pub fn confirm_pending_email(c: &Connection, user_id: i64) -> rusqlite::Result<bool> {
    let Some(email) = user_by_id(c, user_id)?.and_then(|u| u.pending_email) else {
        return Ok(false);
    };
    if email_taken(c, &email, user_id)? {
        return Ok(false);
    }
    set_email(c, user_id, Some(&email))?;
    Ok(true)
}

/// Forgets waiting addresses past their deadline.
pub fn drop_expired_pending_emails(c: &Connection) -> rusqlite::Result<usize> {
    c.execute(
        "UPDATE users SET pending_email = NULL, pending_email_expires_at = NULL
         WHERE pending_email_expires_at IS NOT NULL AND pending_email_expires_at <= ?1",
        params![now_s()],
    )
}

/// Stores a new emailed link for `user_id`, replacing any earlier link for
/// the same purpose (only the newest one works).
pub fn create_email_token(
    c: &Connection,
    hash: &TokenHash,
    user_id: i64,
    purpose: EmailPurpose,
    email: &str,
    ttl_s: i64,
) -> rusqlite::Result<()> {
    let now = now_s();
    drop_expired_pending_emails(c)?;
    c.execute(
        "DELETE FROM email_tokens WHERE expires_at < ?1 OR (user_id = ?2 AND purpose = ?3)",
        params![now, user_id, purpose.as_str()],
    )?;
    c.execute(
        "INSERT INTO email_tokens (token_hash, user_id, purpose, email, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![&hash.0[..], user_id, purpose.as_str(), email, now + ttl_s],
    )?;
    Ok(())
}

/// Uses up a link: returns its user if it exists, has not expired, is for
/// `purpose`, and was sent to the address it is for now (the waiting
/// address for confirmation, the confirmed one for a reset).
pub fn take_email_token(
    c: &Connection,
    hash: &TokenHash,
    purpose: EmailPurpose,
) -> rusqlite::Result<Option<User>> {
    let found: Option<(i64, String, i64)> = c
        .query_row(
            "SELECT user_id, email, expires_at FROM email_tokens WHERE token_hash = ?1 AND purpose = ?2",
            params![&hash.0[..], purpose.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((user_id, email, expires_at)) = found else {
        return Ok(None);
    };
    c.execute(
        "DELETE FROM email_tokens WHERE token_hash = ?1",
        params![&hash.0[..]],
    )?;
    if expires_at <= now_s() {
        return Ok(None);
    }
    let user = user_by_id(c, user_id)?;
    Ok(user.filter(|u| {
        let current = match purpose {
            EmailPurpose::Verify => u.pending_email.as_deref(),
            EmailPurpose::Reset => u.email.as_deref(),
        };
        current.is_some_and(|e| e.eq_ignore_ascii_case(&email))
    }))
}

/// The username a live link belongs to, without using it up.
pub fn email_token_owner(
    c: &Connection,
    hash: &TokenHash,
    purpose: EmailPurpose,
) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT u.username FROM email_tokens t JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = ?1 AND t.purpose = ?2 AND t.expires_at > ?3",
        params![&hash.0[..], purpose.as_str(), now_s()],
        |r| r.get(0),
    )
    .optional()
}

pub fn set_password(c: &Connection, user_id: i64, password_hash: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE users SET password_hash = ?1 WHERE id = ?2",
        params![password_hash, user_id],
    )?;
    // Changing a password signs out every web session.
    c.execute(
        "DELETE FROM web_sessions WHERE user_id = ?1",
        params![user_id],
    )?;
    Ok(())
}

pub fn list_users(c: &Connection) -> rusqlite::Result<Vec<User>> {
    let mut s = c.prepare(&format!(
        "SELECT {USER_COLUMNS} FROM users ORDER BY username COLLATE NOCASE"
    ))?;
    s.query_map([], user_row)?.collect()
}

pub fn delete_user(c: &Connection, username: &str) -> rusqlite::Result<bool> {
    Ok(c.execute("DELETE FROM users WHERE username = ?1", params![username])? > 0)
}

pub fn user_count(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
}

pub fn create_web_session(
    c: &Connection,
    hash: &TokenHash,
    user_id: i64,
    ttl_s: i64,
) -> rusqlite::Result<()> {
    let now = now_s();
    c.execute(
        "DELETE FROM web_sessions WHERE expires_at < ?1",
        params![now],
    )?;
    c.execute(
        "INSERT INTO web_sessions (token_hash, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
        params![&hash.0[..], user_id, now, now + ttl_s],
    )?;
    Ok(())
}

/// The user for a live web session.
pub fn web_session_user(
    c: &Connection,
    hash: &TokenHash,
) -> rusqlite::Result<Option<(i64, String)>> {
    c.query_row(
        "SELECT u.id, u.username FROM web_sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = ?1 AND s.expires_at > ?2",
        params![&hash.0[..], now_s()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

pub fn delete_web_session(c: &Connection, hash: &TokenHash) -> rusqlite::Result<()> {
    c.execute(
        "DELETE FROM web_sessions WHERE token_hash = ?1",
        params![&hash.0[..]],
    )?;
    Ok(())
}

pub fn create_node(
    c: &Connection,
    id: &str,
    user_id: i64,
    name: &str,
    platform: Option<&str>,
    token_hash: &TokenHash,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO nodes (id, user_id, name, platform, token_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, user_id, name, platform, &token_hash.0[..], now_s()],
    )?;
    Ok(())
}

/// The node and owner for a node token.
pub fn node_by_token(
    c: &Connection,
    hash: &TokenHash,
) -> rusqlite::Result<Option<(NodeRow, String)>> {
    c.query_row(
        "SELECT n.id, n.user_id, n.name, n.platform, n.last_seen, u.username
         FROM nodes n JOIN users u ON u.id = n.user_id WHERE n.token_hash = ?1",
        params![&hash.0[..]],
        |r| {
            Ok((
                NodeRow {
                    id: r.get(0)?,
                    user_id: r.get(1)?,
                    name: r.get(2)?,
                    platform: r.get(3)?,
                    last_seen: r.get(4)?,
                },
                r.get(5)?,
            ))
        },
    )
    .optional()
}

pub fn nodes_for_user(c: &Connection, user_id: i64) -> rusqlite::Result<Vec<NodeRow>> {
    let mut s = c.prepare(
        "SELECT id, user_id, name, platform, last_seen FROM nodes WHERE user_id = ?1 ORDER BY name COLLATE NOCASE",
    )?;
    s.query_map(params![user_id], |r| {
        Ok(NodeRow {
            id: r.get(0)?,
            user_id: r.get(1)?,
            name: r.get(2)?,
            platform: r.get(3)?,
            last_seen: r.get(4)?,
        })
    })?
    .collect()
}

pub fn touch_node(c: &Connection, id: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE nodes SET last_seen = ?1 WHERE id = ?2",
        params![now_s(), id],
    )?;
    Ok(())
}

pub fn rename_node(c: &Connection, id: &str, user_id: i64, name: &str) -> rusqlite::Result<bool> {
    Ok(c.execute(
        "UPDATE nodes SET name = ?1 WHERE id = ?2 AND user_id = ?3",
        params![name, id, user_id],
    )? > 0)
}

pub fn delete_node(c: &Connection, id: &str, user_id: i64) -> rusqlite::Result<bool> {
    Ok(c.execute(
        "DELETE FROM nodes WHERE id = ?1 AND user_id = ?2",
        params![id, user_id],
    )? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::hash_token;

    #[test]
    fn users_sessions_nodes_and_codes() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            let uid = create_user(c, "alice", "hash")?;
            assert!(
                create_user(c, "ALICE", "x").is_err(),
                "usernames are case-insensitive"
            );
            assert_eq!(user_by_name(c, "Alice")?.unwrap().id, uid);

            let t = hash_token("session-token");
            create_web_session(c, &t, uid, 60)?;
            assert_eq!(web_session_user(c, &t)?.unwrap().1, "alice");
            set_password(c, uid, "new")?;
            assert!(
                web_session_user(c, &t)?.is_none(),
                "password change signs out"
            );

            let nt = hash_token("node-token");
            create_node(c, "n1", uid, "Studio PC", Some("windows"), &nt)?;
            assert_eq!(node_by_token(c, &nt)?.unwrap().0.name, "Studio PC");
            assert!(rename_node(c, "n1", uid, "Desk")?);
            assert!(
                !rename_node(c, "n1", uid + 1, "Other")?,
                "only the owner may rename"
            );
            assert_eq!(nodes_for_user(c, uid)?.len(), 1);
            assert!(delete_user(c, "alice")?);
            assert!(
                node_by_token(c, &nt)?.is_none(),
                "deleting a user removes their nodes"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn email_addresses_and_links() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            let a = create_user(c, "alice", "hash")?;
            let b = create_user(c, "bob", "hash")?;
            assert_eq!(user_by_name(c, "alice")?.unwrap().email, None);

            // A waiting address blocks no one: both may wait for it.
            set_pending_email(c, a, Some("Shared@Example.com"), Some(now_s() + 60))?;
            assert!(!email_taken(c, "shared@example.com", b)?);
            set_pending_email(c, b, Some("shared@example.com"), Some(now_s() + 60))?;
            assert!(
                user_by_email(c, "shared@example.com")?.is_none(),
                "not confirmed"
            );

            // The first to confirm gets it; the other stops waiting for it.
            let t = hash_token("verify-link");
            create_email_token(c, &t, a, EmailPurpose::Verify, "Shared@Example.com", 60)?;
            assert!(
                take_email_token(c, &t, EmailPurpose::Reset)?.is_none(),
                "wrong purpose"
            );
            let tb = hash_token("verify-b");
            create_email_token(c, &tb, b, EmailPurpose::Verify, "shared@example.com", 60)?;
            let u = take_email_token(c, &t, EmailPurpose::Verify)?.unwrap();
            assert!(confirm_pending_email(c, u.id)?);
            let alice = user_by_id(c, a)?.unwrap();
            assert_eq!(
                (alice.email.as_deref(), alice.pending_email),
                (Some("Shared@Example.com"), None)
            );
            assert_eq!(user_by_id(c, b)?.unwrap().pending_email, None);
            assert!(
                take_email_token(c, &tb, EmailPurpose::Verify)?.is_none(),
                "bob's link is for an address he no longer waits for"
            );
            assert!(
                take_email_token(c, &t, EmailPurpose::Verify)?.is_none(),
                "single use"
            );
            assert!(email_taken(c, "SHARED@example.com", b)?);
            // A confirmed address is still unique.
            assert!(set_email(c, b, Some("shared@example.com")).is_err());

            // Changing: the confirmed address keeps working for resets until
            // the new one is confirmed.
            let r1 = hash_token("reset-1");
            let r2 = hash_token("reset-2");
            create_email_token(c, &r1, a, EmailPurpose::Reset, "shared@example.com", 60)?;
            create_email_token(c, &r2, a, EmailPurpose::Reset, "shared@example.com", 60)?;
            assert!(
                take_email_token(c, &r1, EmailPurpose::Reset)?.is_none(),
                "newest only"
            );
            set_pending_email(c, a, Some("new@example.com"), Some(now_s() + 60))?;
            assert_eq!(user_by_email(c, "shared@example.com")?.unwrap().id, a);
            assert!(take_email_token(c, &r2, EmailPurpose::Reset)?.is_some());

            // Expired links and expired waiting addresses do not count.
            let old = hash_token("old");
            create_email_token(c, &old, a, EmailPurpose::Reset, "shared@example.com", -1)?;
            assert!(take_email_token(c, &old, EmailPurpose::Reset)?.is_none());
            set_pending_email(c, b, Some("late@example.com"), Some(now_s() - 1))?;
            assert_eq!(user_by_id(c, b)?.unwrap().pending_email, None);
            assert_eq!(drop_expired_pending_emails(c)?, 1);
            // Without a deadline (a server without email), it stays.
            set_pending_email(c, b, Some("kept@example.com"), None)?;
            assert_eq!(drop_expired_pending_emails(c)?, 0);
            assert!(user_by_id(c, b)?.unwrap().pending_email.is_some());

            // Several accounts may have no confirmed address.
            set_email(c, a, None)?;
            assert!(!email_taken(c, "shared@example.com", b)?);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn upgrades_a_version_2_database() {
        let dir = std::env::temp_dir().join(format!("audionet-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v2.db");
        let _ = std::fs::remove_file(&path);
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL UNIQUE COLLATE NOCASE, password_hash TEXT NOT NULL, created_at INTEGER NOT NULL);
                 CREATE TABLE web_sessions (token_hash BLOB PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
                 CREATE TABLE nodes (id TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, name TEXT NOT NULL, platform TEXT, token_hash BLOB NOT NULL UNIQUE, created_at INTEGER NOT NULL, last_seen INTEGER);
                 INSERT INTO users (username, password_hash, created_at) VALUES ('old', 'h', 0);
                 PRAGMA user_version = 2;",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let u = db.with(|c| user_by_name(c, "old")).unwrap().unwrap();
        assert_eq!((u.email, u.pending_email), (None, None));
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
