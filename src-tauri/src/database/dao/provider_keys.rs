use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::provider::{
    ProviderConfigKeyBinding, ProviderKey, ProviderKeyInput, ProviderKeyStatus, ProviderKeySummary,
    UsageScript,
};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Row};
use uuid::Uuid;

fn now_ts() -> i64 {
    Utc::now().timestamp()
}

fn normalize_config_key_mode(mode: &str) -> Result<&'static str, AppError> {
    match mode.trim() {
        "auto" => Ok("auto"),
        "manual" => Ok("manual"),
        other => Err(AppError::Database(format!(
            "Invalid provider config key binding mode: {other}"
        ))),
    }
}

fn map_provider_key_row(row: &Row<'_>) -> rusqlite::Result<ProviderKey> {
    let status: String = row.get(9)?;
    let usage_script_raw: Option<String> = row.get(17)?;
    Ok(ProviderKey {
        id: row.get(0)?,
        app_type: row.get(1)?,
        provider_id: row.get(2)?,
        name: row.get(3)?,
        key_value: row.get(4)?,
        auth_field: row.get(5)?,
        enabled: row.get::<_, i64>(6)? != 0,
        priority: row.get(7)?,
        weight: row.get(8)?,
        status: ProviderKeyStatus::from(status.as_str()),
        consecutive_failures: row.get(10)?,
        last_success_at: row.get(11)?,
        last_failure_at: row.get(12)?,
        last_used_at: row.get(13)?,
        cooldown_until: row.get(14)?,
        usage_script: usage_script_raw
            .filter(|s| !s.trim().is_empty())
            .and_then(|s| serde_json::from_str(&s).ok()),
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

impl Database {
    pub fn get_provider_config_key_binding(
        &self,
        app_type: &str,
        provider_id: &str,
    ) -> Result<Option<ProviderConfigKeyBinding>, AppError> {
        let conn = lock_conn!(self.conn);
        conn.query_row(
            "SELECT app_type, provider_id, key_id, mode, created_at, updated_at
             FROM provider_config_key_bindings
             WHERE app_type = ?1 AND provider_id = ?2",
            params![app_type, provider_id],
            |row| {
                Ok(ProviderConfigKeyBinding {
                    app_type: row.get(0)?,
                    provider_id: row.get(1)?,
                    key_id: row.get(2)?,
                    mode: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|e| AppError::Database(e.to_string()))
    }

    pub fn set_provider_config_key_binding(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: &str,
        mode: &str,
    ) -> Result<(), AppError> {
        let mode = normalize_config_key_mode(mode)?;
        let now = now_ts();
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO provider_config_key_bindings (
                app_type, provider_id, key_id, mode, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ON CONFLICT(app_type, provider_id) DO UPDATE SET
                key_id = excluded.key_id,
                mode = excluded.mode,
                updated_at = excluded.updated_at",
            params![app_type, provider_id, key_id, mode, now],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn clear_provider_config_key_binding(
        &self,
        app_type: &str,
        provider_id: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM provider_config_key_bindings
             WHERE app_type = ?1 AND provider_id = ?2",
            params![app_type, provider_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn get_provider_key_summaries(
        &self,
        app_type: &str,
    ) -> Result<Vec<ProviderKeySummary>, AppError> {
        let now = now_ts();
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT app_type,
                        provider_id,
                        COUNT(1) AS total,
                        SUM(CASE
                              WHEN enabled = 1
                               AND status != 'disabled'
                               AND (cooldown_until IS NULL OR cooldown_until <= ?2)
                              THEN 1 ELSE 0
                            END) AS available,
                        SUM(CASE WHEN enabled = 1 AND status = 'degraded' THEN 1 ELSE 0 END) AS degraded,
                        SUM(CASE WHEN enabled = 1 AND status = 'cooldown' THEN 1 ELSE 0 END) AS cooldown,
                        SUM(CASE WHEN enabled = 0 OR status = 'disabled' THEN 1 ELSE 0 END) AS disabled,
                        MIN(CASE WHEN enabled = 1 AND status != 'disabled' THEN priority ELSE NULL END) AS min_priority,
                        SUM(CASE WHEN usage_script IS NOT NULL
                                  AND json_valid(usage_script)
                                  AND json_extract(usage_script, '$.enabled') = 1
                             THEN 1 ELSE 0 END) AS usage_enabled
                 FROM provider_keys
                 WHERE app_type = ?1
                 GROUP BY app_type, provider_id",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let summaries = stmt
            .query_map(params![app_type, now], |row| {
                Ok(ProviderKeySummary {
                    app_type: row.get(0)?,
                    provider_id: row.get(1)?,
                    total: row.get(2)?,
                    available: row.get(3)?,
                    degraded: row.get(4)?,
                    cooldown: row.get(5)?,
                    disabled: row.get(6)?,
                    min_priority: row.get(7)?,
                    usage_enabled: row.get(8)?,
                })
            })
            .map_err(|e| AppError::Database(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(summaries)
    }

    pub fn get_provider_keys(
        &self,
        app_type: &str,
        provider_id: &str,
    ) -> Result<Vec<ProviderKey>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT id, app_type, provider_id, name, key_value, auth_field,
                        enabled, priority, weight, status, consecutive_failures,
                        last_success_at, last_failure_at, last_used_at, cooldown_until,
                        created_at, updated_at, usage_script
                 FROM provider_keys
                 WHERE app_type = ?1 AND provider_id = ?2
                 ORDER BY enabled DESC, priority ASC, created_at ASC, id ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let keys = stmt
            .query_map(params![app_type, provider_id], map_provider_key_row)
            .map_err(|e| AppError::Database(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(keys)
    }

    pub fn get_enabled_provider_keys(
        &self,
        app_type: &str,
        provider_id: &str,
    ) -> Result<Vec<ProviderKey>, AppError> {
        let now = now_ts();
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT id, app_type, provider_id, name, key_value, auth_field,
                        enabled, priority, weight, status, consecutive_failures,
                        last_success_at, last_failure_at, last_used_at, cooldown_until,
                        created_at, updated_at, usage_script
                 FROM provider_keys
                 WHERE app_type = ?1
                   AND provider_id = ?2
                   AND enabled = 1
                   AND status != 'disabled'
                   AND (cooldown_until IS NULL OR cooldown_until <= ?3)
                 ORDER BY priority ASC, consecutive_failures ASC,
                          COALESCE(last_success_at, 0) DESC,
                          COALESCE(last_used_at, 0) ASC,
                          created_at ASC, id ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let keys = stmt
            .query_map(params![app_type, provider_id, now], map_provider_key_row)
            .map_err(|e| AppError::Database(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(keys)
    }

    pub fn has_provider_keys(&self, app_type: &str, provider_id: &str) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(1)
                 FROM provider_keys
                 WHERE app_type = ?1 AND provider_id = ?2",
                params![app_type, provider_id],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(count > 0)
    }

    pub fn get_provider_key(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: &str,
    ) -> Result<Option<ProviderKey>, AppError> {
        let conn = lock_conn!(self.conn);
        conn.query_row(
            "SELECT id, app_type, provider_id, name, key_value, auth_field,
                    enabled, priority, weight, status, consecutive_failures,
                    last_success_at, last_failure_at, last_used_at, cooldown_until,
                    created_at, updated_at, usage_script
             FROM provider_keys
             WHERE id = ?1 AND app_type = ?2 AND provider_id = ?3",
            params![key_id, app_type, provider_id],
            map_provider_key_row,
        )
        .optional()
        .map_err(|e| AppError::Database(e.to_string()))
    }

    pub fn add_provider_key(
        &self,
        app_type: &str,
        provider_id: &str,
        input: &ProviderKeyInput,
    ) -> Result<ProviderKey, AppError> {
        let key = ProviderKey {
            id: Uuid::new_v4().to_string(),
            app_type: app_type.to_string(),
            provider_id: provider_id.to_string(),
            name: input.name.trim().to_string(),
            key_value: input.key_value.trim().to_string(),
            auth_field: input.auth_field.clone().filter(|v| !v.trim().is_empty()),
            enabled: input.enabled,
            priority: input.priority,
            weight: input.weight.max(1),
            status: if input.enabled {
                ProviderKeyStatus::Active
            } else {
                ProviderKeyStatus::Disabled
            },
            consecutive_failures: 0,
            last_success_at: None,
            last_failure_at: None,
            last_used_at: None,
            cooldown_until: None,
            usage_script: input.usage_script.clone(),
            created_at: now_ts(),
            updated_at: now_ts(),
        };
        self.save_provider_key(&key)?;
        Ok(key)
    }

    pub fn save_provider_key(&self, key: &ProviderKey) -> Result<(), AppError> {
        let usage_script_json = match &key.usage_script {
            Some(s) => Some(
                serde_json::to_string(s)
                    .map_err(|e| AppError::Database(format!("serialize usage_script: {e}")))?,
            ),
            None => None,
        };
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT OR REPLACE INTO provider_keys (
                id, app_type, provider_id, name, key_value, auth_field,
                enabled, priority, weight, status, consecutive_failures,
                last_success_at, last_failure_at, last_used_at, cooldown_until,
                created_at, updated_at, usage_script
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                key.id,
                key.app_type,
                key.provider_id,
                key.name,
                key.key_value,
                key.auth_field,
                if key.enabled { 1 } else { 0 },
                key.priority,
                key.weight.max(1),
                key.status.as_str(),
                key.consecutive_failures,
                key.last_success_at,
                key.last_failure_at,
                key.last_used_at,
                key.cooldown_until,
                key.created_at,
                key.updated_at,
                usage_script_json,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_provider_key(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: &str,
        input: &ProviderKeyInput,
    ) -> Result<Option<ProviderKey>, AppError> {
        let Some(mut key) = self.get_provider_key(app_type, provider_id, key_id)? else {
            return Ok(None);
        };

        key.name = input.name.trim().to_string();
        key.key_value = input.key_value.trim().to_string();
        key.auth_field = input.auth_field.clone().filter(|v| !v.trim().is_empty());
        key.enabled = input.enabled;
        key.priority = input.priority;
        key.weight = input.weight.max(1);
        key.status = if input.enabled {
            match key.status {
                ProviderKeyStatus::Disabled => ProviderKeyStatus::Active,
                status => status,
            }
        } else {
            ProviderKeyStatus::Disabled
        };
        key.updated_at = now_ts();
        self.save_provider_key(&key)?;
        Ok(Some(key))
    }

    /// 仅更新某个 key 的用量查询配置（不触碰 key 的其他字段，
    /// 与「切 key / 调优先级」等操作隔离，避免配置漂移）。
    /// 传 None 表示清除该 key 的用量配置。
    pub fn set_provider_key_usage_script(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: &str,
        usage_script: Option<&UsageScript>,
    ) -> Result<Option<ProviderKey>, AppError> {
        let Some(mut key) = self.get_provider_key(app_type, provider_id, key_id)? else {
            return Ok(None);
        };
        key.usage_script = usage_script.cloned();
        key.updated_at = now_ts();
        self.save_provider_key(&key)?;
        Ok(Some(key))
    }

    pub fn delete_provider_key(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: &str,
    ) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let deleted = conn
            .execute(
                "DELETE FROM provider_keys
                 WHERE id = ?1 AND app_type = ?2 AND provider_id = ?3",
                params![key_id, app_type, provider_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(deleted > 0)
    }

    pub fn reset_provider_key_health(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: &str,
    ) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let updated = conn
            .execute(
                "UPDATE provider_keys
                 SET status = CASE WHEN enabled = 1 THEN 'active' ELSE 'disabled' END,
                     consecutive_failures = 0,
                     last_failure_at = NULL,
                     cooldown_until = NULL,
                     updated_at = ?4
                 WHERE id = ?1 AND app_type = ?2 AND provider_id = ?3",
                params![key_id, app_type, provider_id, now_ts()],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(updated > 0)
    }

    /// 一键重置某供应商全部 Key 的健康状态（清冷却/失败计数），返回受影响行数。
    pub fn reset_all_provider_keys_health(
        &self,
        app_type: &str,
        provider_id: &str,
    ) -> Result<u64, AppError> {
        let conn = lock_conn!(self.conn);
        let updated = conn
            .execute(
                "UPDATE provider_keys
                 SET status = CASE WHEN enabled = 1 THEN 'active' ELSE 'disabled' END,
                     consecutive_failures = 0,
                     last_failure_at = NULL,
                     cooldown_until = NULL,
                     updated_at = ?3
                 WHERE app_type = ?1 AND provider_id = ?2",
                params![app_type, provider_id, now_ts()],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(updated as u64)
    }

    /// 禁用与指定 Key 相同 `key_value` 的全部 Key 池条目，跨应用和供应商生效。
    ///
    /// 403 通常说明该密钥本身不可用；同一个密钥可能被配置在多个 provider 下，
    /// 因此先通过当前请求命中的 key_id 找到明文值，再禁用所有匹配项。
    /// 这里不限制 app_type/provider_id，确保 Claude、Codex、Gemini 或同应用内
    /// 多个供应商复用同一密钥时不会继续轮询到已经被拒绝的 key。
    pub fn disable_provider_keys_matching_key_id(&self, key_id: &str) -> Result<u64, AppError> {
        let now = now_ts();
        let conn = lock_conn!(self.conn);
        let key_value: Option<String> = conn
            .query_row(
                "SELECT key_value FROM provider_keys WHERE id = ?1",
                params![key_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?;

        let Some(key_value) = key_value else {
            return Ok(0);
        };

        let updated = conn
            .execute(
                "UPDATE provider_keys
                 SET enabled = 0,
                     status = 'disabled',
                     last_failure_at = ?2,
                     last_used_at = ?2,
                     cooldown_until = NULL,
                     updated_at = ?2
                 WHERE key_value = ?1
                   AND (enabled != 0 OR status != 'disabled')",
                params![key_value, now],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(updated as u64)
    }

    /// 该应用下最早一个冷却中 Key 距恢复还有多少秒（用于 503 响应的 Retry-After）。
    ///
    /// 只统计启用且未停用的 Key；没有任何 Key 在冷却时返回 None。
    pub fn earliest_provider_key_recovery_secs(
        &self,
        app_type: &str,
    ) -> Result<Option<u64>, AppError> {
        let now = now_ts();
        let conn = lock_conn!(self.conn);
        let earliest: Option<i64> = conn
            .query_row(
                "SELECT MIN(cooldown_until) FROM provider_keys
                 WHERE app_type = ?1
                   AND enabled = 1
                   AND status != 'disabled'
                   AND cooldown_until > ?2",
                params![app_type, now],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(earliest.map(|until| (until - now).max(1) as u64))
    }

    pub fn record_provider_key_success(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: &str,
    ) -> Result<bool, AppError> {
        // 代理热路径每个请求都会落一次健康状态；provider_keys 在自动同步
        // 触发白名单里，这类运行时写入必须抑制，否则会引发同步风暴。
        let _webdav_guard = crate::services::webdav_auto_sync::AutoSyncSuppressionGuard::new();
        let _s3_guard = crate::services::s3_auto_sync::AutoSyncSuppressionGuard::new();
        let now = now_ts();
        let conn = lock_conn!(self.conn);
        let updated = conn
            .execute(
                "UPDATE provider_keys
                 SET status = CASE WHEN enabled = 1 THEN 'active' ELSE 'disabled' END,
                     consecutive_failures = 0,
                     last_success_at = ?4,
                     last_used_at = ?4,
                     cooldown_until = NULL,
                     updated_at = ?4
                 WHERE id = ?1 AND app_type = ?2 AND provider_id = ?3",
                params![key_id, app_type, provider_id, now],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(updated > 0)
    }

    /// 记录 Key 失败并按指数退避计算冷却时长（ccLoad 风格）。
    ///
    /// 冷却秒数 = min(base × 2^(已有连续失败数 - 宽限), cap)，成功后由
    /// `record_provider_key_success` 清零。`base_seconds <= 0` 表示不冷却，
    /// 仅标记 Degraded。
    ///
    /// `grace_failures` 为冷却宽限：已有连续失败数未达到该值时不进入冷却，
    /// 只标 Degraded（留在轮转中，组内按失败数降序靠后）。传 0 即原有行为。
    pub fn record_provider_key_failure(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: &str,
        cooldown_base_seconds: i64,
        cooldown_cap_seconds: i64,
        grace_failures: i64,
    ) -> Result<bool, AppError> {
        // 同 record_provider_key_success：运行时健康写入不触发自动同步。
        let _webdav_guard = crate::services::webdav_auto_sync::AutoSyncSuppressionGuard::new();
        let _s3_guard = crate::services::s3_auto_sync::AutoSyncSuppressionGuard::new();
        let now = now_ts();
        let conn = lock_conn!(self.conn);

        let prior_failures: i64 = conn
            .query_row(
                "SELECT consecutive_failures FROM provider_keys
                 WHERE id = ?1 AND app_type = ?2 AND provider_id = ?3",
                params![key_id, app_type, provider_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let cooldown_until = if cooldown_base_seconds > 0 && prior_failures >= grace_failures {
            let exponent = (prior_failures - grace_failures).clamp(0, 16) as u32;
            let backoff = cooldown_base_seconds
                .saturating_mul(1i64 << exponent)
                .min(cooldown_cap_seconds.max(cooldown_base_seconds));
            Some(now + backoff)
        } else {
            None
        };
        let status = if cooldown_until.is_some() {
            ProviderKeyStatus::Cooldown
        } else {
            ProviderKeyStatus::Degraded
        };

        let updated = conn
            .execute(
                "UPDATE provider_keys
                 SET status = ?4,
                     consecutive_failures = consecutive_failures + 1,
                     last_failure_at = ?5,
                     last_used_at = ?5,
                     cooldown_until = ?6,
                     updated_at = ?5
                 WHERE id = ?1 AND app_type = ?2 AND provider_id = ?3",
                params![
                    key_id,
                    app_type,
                    provider_id,
                    status.as_str(),
                    now,
                    cooldown_until,
                ],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(updated > 0)
    }
}
