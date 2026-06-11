use crate::database::{lock_conn, Database};
use crate::error::AppError;
use chrono::Utc;
use rusqlite::{params, OptionalExtension};

const DEFAULT_SESSION_AFFINITY_TTL_SECONDS: i64 = 24 * 60 * 60;
const DEFAULT_WORKING_CHANNEL_AFFINITY_TTL_SECONDS: i64 = 30 * 60;

fn now_ts() -> i64 {
    Utc::now().timestamp()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAffinity {
    pub app_type: String,
    pub session_id: String,
    pub provider_id: String,
    pub key_id: Option<String>,
    pub expires_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingChannelAffinity {
    pub app_type: String,
    pub provider_id: String,
    pub key_id: Option<String>,
    pub expires_at: i64,
    pub updated_at: i64,
}

impl Database {
    pub fn get_session_affinity(
        &self,
        app_type: &str,
        session_id: &str,
    ) -> Result<Option<SessionAffinity>, AppError> {
        let now = now_ts();
        let conn = lock_conn!(self.conn);
        conn.query_row(
            "SELECT app_type, session_id, provider_id, key_id, expires_at, updated_at
             FROM session_affinity
             WHERE app_type = ?1 AND session_id = ?2 AND expires_at > ?3",
            params![app_type, session_id, now],
            |row| {
                Ok(SessionAffinity {
                    app_type: row.get(0)?,
                    session_id: row.get(1)?,
                    provider_id: row.get(2)?,
                    key_id: row.get(3)?,
                    expires_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|e| AppError::Database(e.to_string()))
    }

    pub fn upsert_session_affinity(
        &self,
        app_type: &str,
        session_id: &str,
        provider_id: &str,
        key_id: Option<&str>,
    ) -> Result<(), AppError> {
        if session_id.trim().is_empty() {
            return Ok(());
        }

        let now = now_ts();
        let expires_at = now + DEFAULT_SESSION_AFFINITY_TTL_SECONDS;
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO session_affinity (
                app_type, session_id, provider_id, key_id, expires_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(app_type, session_id) DO UPDATE SET
                provider_id = excluded.provider_id,
                key_id = excluded.key_id,
                expires_at = excluded.expires_at,
                updated_at = excluded.updated_at",
            params![app_type, session_id, provider_id, key_id, expires_at, now],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn delete_session_affinity(
        &self,
        app_type: &str,
        session_id: &str,
    ) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let deleted = conn
            .execute(
                "DELETE FROM session_affinity WHERE app_type = ?1 AND session_id = ?2",
                params![app_type, session_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(deleted > 0)
    }

    pub fn delete_session_affinity_if_matches(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: Option<&str>,
    ) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let deleted = conn
            .execute(
                "DELETE FROM session_affinity
                 WHERE app_type = ?1
                   AND provider_id = ?2
                   AND (
                     (key_id IS NULL AND ?3 IS NULL)
                     OR key_id = ?3
                   )",
                params![app_type, provider_id, key_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(deleted > 0)
    }

    pub fn get_working_channel_affinity(
        &self,
        app_type: &str,
    ) -> Result<Option<WorkingChannelAffinity>, AppError> {
        let now = now_ts();
        let conn = lock_conn!(self.conn);
        conn.query_row(
            "SELECT app_type, provider_id, key_id, expires_at, updated_at
             FROM working_channel_affinity
             WHERE app_type = ?1 AND expires_at > ?2",
            params![app_type, now],
            |row| {
                Ok(WorkingChannelAffinity {
                    app_type: row.get(0)?,
                    provider_id: row.get(1)?,
                    key_id: row.get(2)?,
                    expires_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|e| AppError::Database(e.to_string()))
    }

    pub fn upsert_working_channel_affinity(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: Option<&str>,
    ) -> Result<(), AppError> {
        let now = now_ts();
        let expires_at = now + DEFAULT_WORKING_CHANNEL_AFFINITY_TTL_SECONDS;
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO working_channel_affinity (
                app_type, provider_id, key_id, expires_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(app_type) DO UPDATE SET
                provider_id = excluded.provider_id,
                key_id = excluded.key_id,
                expires_at = excluded.expires_at,
                updated_at = excluded.updated_at",
            params![app_type, provider_id, key_id, expires_at, now],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn delete_working_channel_affinity_if_matches(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: Option<&str>,
    ) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let deleted = conn
            .execute(
                "DELETE FROM working_channel_affinity
                 WHERE app_type = ?1
                   AND provider_id = ?2
                   AND (
                     (key_id IS NULL AND ?3 IS NULL)
                     OR key_id = ?3
                   )",
                params![app_type, provider_id, key_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(deleted > 0)
    }
}
