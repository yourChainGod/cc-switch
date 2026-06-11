# Local Gateway Roadmap

## Goal

Turn cc-switch into a stronger local desktop gateway while keeping the existing
Tauri desktop workflow and provider compatibility.

The target behavior is:

- one local desktop proxy for Claude Code, Codex, Gemini CLI, Claude Desktop,
  OpenCode, OpenClaw, and Hermes;
- providers can own a pool of API keys or account tokens;
- retry/failover works at `provider:key` channel level, not only provider level;
- successful channels are remembered per session and reused until they fail or
  expire;
- optional check-in/account-token maintenance can manage NewAPI/OneAPI-style
  aggregation site accounts and inspect their API tokens without treating site
  access tokens as LLM provider keys;
- promotional partner presets and coupon copy are removed from the UI.

## Current Baseline

cc-switch already has the right base:

- Tauri desktop shell and tray integration.
- Local proxy server with per-app takeover.
- Provider queue and circuit breaker.
- Request retry and error classification.
- Session ID extraction for logging.
- SQLite-backed provider, health, proxy, usage, and stream-check tables.

Main gaps:

- Provider credentials are embedded in heterogeneous `settingsConfig` JSON
  shapes, so each provider effectively has one key.
- Health and circuit breaker state are keyed by provider only.
- Session ID is logged but not used to prefer previously successful channels.
- Check-in/account maintenance is not modeled.

## Phase 1: Clean Presets

Remove promotional surface without changing proxy behavior.

Tasks:

- Remove partner badges and promotion copy from i18n.
- Strip affiliate/utm query parameters from preset URLs.
- Keep neutral provider templates only.
- Preserve custom provider creation.

Validation:

- TypeScript build passes.
- Provider creation still works from neutral presets.

## Phase 2: Provider Key Pool

Add a normalized key pool next to the current provider table.

Proposed table:

```sql
provider_keys (
  id TEXT PRIMARY KEY,
  app_type TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  name TEXT NOT NULL,
  key_value TEXT NOT NULL,
  auth_field TEXT,
  enabled INTEGER NOT NULL DEFAULT 1,
  priority INTEGER NOT NULL DEFAULT 0,
  weight INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL DEFAULT 'active',
  consecutive_failures INTEGER NOT NULL DEFAULT 0,
  last_success_at INTEGER,
  last_failure_at INTEGER,
  last_used_at INTEGER,
  cooldown_until INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY (provider_id, app_type)
    REFERENCES providers(id, app_type) ON DELETE CASCADE
)
```

Important compatibility rule:

- Existing providers without `provider_keys` still use their embedded key.
- A provider with enabled `provider_keys` uses the key pool first.

Validation:

- Fresh database creates the table.
- Existing database migrates from schema version 10 to 11.
- Existing provider CRUD remains compatible.

## Phase 3: Key CRUD APIs

Expose key-pool operations to the frontend.

Backend commands:

- `get_provider_keys(app_type, provider_id)`
- `add_provider_key(app_type, provider_id, input)`
- `update_provider_key(app_type, provider_id, key_id, input)`
- `delete_provider_key(app_type, provider_id, key_id)`
- `reset_provider_key_health(app_type, provider_id, key_id)`

Validation:

- Unit tests for DAO insert/update/delete/order.
- Frontend API wrappers compile.

## Phase 4: Key Management UI

Add provider-card and edit-dialog key-pool management.

UI behavior:

- A provider shows key count and degraded/disabled count.
- Edit provider panel has a "Keys" section.
- Users can paste multiple keys, one per line.
- Existing single key can be imported into the pool.
- Keys are masked by default.

Validation:

- TypeScript build passes.
- Empty key pool keeps legacy behavior.

## Phase 5: Channel-Level Routing

Introduce `ProviderAttempt = Provider + ProviderKey`.

Routing behavior:

- Build attempts from failover provider queue.
- For each provider, expand enabled keys ordered by priority/health.
- If no keys exist, use the provider's legacy embedded key as one implicit
  attempt.
- Record success/failure on both provider and key.
- Cool down only the failed key when the error is credential/quota/rate-limit
  scoped.
- Cool down the provider when the error is endpoint/provider scoped.

Validation:

- Retry tries another key before another provider when configured.
- Existing provider-level failover still works.
- Request logs include key id when available.

## Phase 6: Session Affinity

Persist the successful channel for client-provided sessions.

Proposed table:

```sql
session_affinity (
  app_type TEXT NOT NULL,
  session_id TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  key_id TEXT,
  expires_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (app_type, session_id)
)
```

Behavior:

- Only bind when the session id came from the client.
- On success, bind or refresh `provider:key`.
- On retry success, replace the binding.
- On failure, skip stale binding and continue through normal channel order.

Validation:

- Same session reuses successful channel.
- Failed bound channel is bypassed after cooldown.

## Phase 7: Gateway Account Maintenance

Port a minimal Metapi-style adapter layer.

Scope for first iteration:

- NewAPI/OneAPI-compatible site account records.
- Manual access token storage.
- `checkin()` and `getApiTokens()`.
- Scheduled or manual check-in.
- API token list/detect for account verification only; do not write detected
  site API tokens into `provider_keys` automatically.

Out of scope for first iteration:

- Full Metapi route engine.
- Multi-user web management.
- Complex OAuth route units.
- Balance dashboards and account quota history.

Validation:

- Manual check-in logs success/failure.
- Check-in schedule stores enabled state, interval, and preferred time.
- API token detection shows masked token details and never creates provider keys.

## Phase 8: Verification

Minimum checks before each larger phase:

- `npm run typecheck`
- `npm run test:unit`
- `cargo test`

Release criteria:

- Clean build.
- Existing provider switching works.
- Local proxy starts and answers health check.
- Legacy single-key provider still routes requests.

## Current Implementation Status

Completed in this branch:

- Phase 1 application cleanup:
  - removed partner badges and promotion cards from provider UI;
  - removed `isPartner` / `partnerPromotionKey` values from provider presets;
  - stripped `aff`, `ref`, `ch`, `from`, and `utm_*` tracking params from preset URLs;
  - removed provider promotion copy from locale JSON files.
- Phase 2 foundation:
  - bumped SQLite schema to v11;
  - added `provider_keys` table and indexes;
  - added Rust provider key data types;
  - added provider key DAO methods for CRUD, enabled-key listing, success/failure recording, and health reset.
- Phase 3 API:
  - added Tauri commands for provider key CRUD;
  - added frontend API wrappers and TypeScript types.
- Phase 4 initial UI:
  - added a Key Pool section to the provider edit panel;
  - supports listing, bulk paste add, enable/disable, delete, and health reset.
  - supports importing an existing embedded single key into an empty key pool.
- Phase 5 channel routing:
  - expands providers into `ProviderAttempt { provider, key_id }`;
  - injects selected key values into provider configs before forwarding;
  - records key-level success/failure and cools down key-scoped errors without poisoning provider health;
  - keeps legacy embedded-key providers working when no key pool exists;
  - records `provider_key_id` into request logs when available.
- Phase 6 session affinity:
  - added `session_affinity` table and DAO;
  - only uses affinity for client-provided sessions;
  - refreshes successful `provider:key` bindings and prioritizes bound attempts on later requests;
  - stale/cooldown key bindings are naturally skipped because unavailable keys are not expanded into attempts.

Not implemented yet:

- Balance dashboards and account quota history;
- README and release-note sponsor cleanup.
