//! SQLite storage: users, web sign-in sessions and devices (nodes).
//!
//! Secrets are never stored directly: passwords are argon2id hashes and all
//! tokens are stored as SHA-256 hashes.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, params};

use crate::auth::TokenHash;

const SCHEMA_VERSION: i64 = 2;

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

pub fn user_by_name(c: &Connection, username: &str) -> rusqlite::Result<Option<User>> {
    c.query_row(
        "SELECT id, username, password_hash FROM users WHERE username = ?1",
        params![username],
        |r| {
            Ok(User {
                id: r.get(0)?,
                username: r.get(1)?,
                password_hash: r.get(2)?,
            })
        },
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

pub fn list_users(c: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut s = c.prepare("SELECT username FROM users ORDER BY username")?;
    s.query_map([], |r| r.get(0))?.collect()
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
}
