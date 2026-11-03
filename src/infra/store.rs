//! SQLite persistence (`SqliteStore` in WAL mode) for `EngineState`, source cursors, and runtime settings.

use crate::core::engine::EngineState;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::Mutex;

/// Thread-safe SQLite store backing the Nexus daemon.
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Opens or creates the SQLite database at `db_path` in WAL mode and runs schema migrations.
    pub fn open(db_path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS engine_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                state_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS source_cursors (
                source_id TEXT NOT NULL,
                cursor_key TEXT NOT NULL,
                cursor_val TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (source_id, cursor_key)
            );
            CREATE TABLE IF NOT EXISTS runtime_settings (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Opens an in-memory SQLite store (useful for fast deterministic unit and integration tests).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE engine_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                state_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE source_cursors (
                source_id TEXT NOT NULL,
                cursor_key TEXT NOT NULL,
                cursor_val TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (source_id, cursor_key)
            );
            CREATE TABLE runtime_settings (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Loads the persisted `EngineState`, or returns `None` if the database is freshly created.
    pub fn load_engine_state(&self) -> anyhow::Result<Option<EngineState>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT state_json FROM engine_state WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match raw {
            Some(json_str) => Ok(Some(serde_json::from_str(&json_str)?)),
            None => Ok(None),
        }
    }

    /// Atomically persists the full `EngineState` snapshot.
    pub fn save_engine_state(&self, state: &EngineState, now: &str) -> anyhow::Result<()> {
        let json_str = serde_json::to_string(state)?;
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT INTO engine_state (id, state_json, updated_at)
             VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET state_json = excluded.state_json, updated_at = excluded.updated_at",
            params![json_str, now],
        )?;
        Ok(())
    }

    /// Reads a source polling cursor (`(source_id, cursor_key)`).
    pub fn get_cursor(&self, source_id: &str, cursor_key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let val = conn
            .query_row(
                "SELECT cursor_val FROM source_cursors WHERE source_id = ?1 AND cursor_key = ?2",
                params![source_id, cursor_key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(val)
    }

    /// Updates a source polling cursor (`(source_id, cursor_key) -> cursor_val`).
    pub fn set_cursor(
        &self,
        source_id: &str,
        cursor_key: &str,
        cursor_val: &str,
        now: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT INTO source_cursors (source_id, cursor_key, cursor_val, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source_id, cursor_key) DO UPDATE SET
               cursor_val = excluded.cursor_val,
               updated_at = excluded.updated_at",
            params![source_id, cursor_key, cursor_val, now],
        )?;
        Ok(())
    }

    /// Reads a JSON runtime setting by `key`.
    pub fn get_setting(&self, key: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT value_json FROM runtime_settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        match raw {
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    /// Writes a JSON runtime setting by `key`.
    pub fn set_setting(
        &self,
        key: &str,
        value: &serde_json::Value,
        now: &str,
    ) -> anyhow::Result<()> {
        let json_str = serde_json::to_string(value)?;
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT INTO runtime_settings (key, value_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET
               value_json = excluded.value_json,
               updated_at = excluded.updated_at",
            params![key, json_str, now],
        )?;
        Ok(())
    }
}
