# 004 — Persistence, Credential Shims & Watchdog (`src/infra/`)

## 1. SQLite Transactional Store (`src/infra/store.rs`)

Nexus persists all state in `<state_dir>/nexus.db` using SQLite in `WAL` (`Write-Ahead Logging`) mode:

- `engine_state (id INTEGER PRIMARY KEY CHECK (id = 1), state_json TEXT NOT NULL, updated_at TEXT NOT NULL)`
- `source_cursors (source_id TEXT NOT NULL, cursor_key TEXT NOT NULL, cursor_val TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY (source_id, cursor_key))`
- `runtime_settings (key TEXT PRIMARY KEY, value_json TEXT NOT NULL, updated_at TEXT NOT NULL)`

Every domain transition (`receive_trigger`, `accept_ticket`, `switch_execution_target`, `record_worker_report`, `thread_closed`) commits atomically to `engine_state` together with any queued `OutboxMessage` and `Effect` records.

---

## 2. Credential-Isolated Shims (`src/infra/shims.rs`)

Workers run with raw `*_SECRET*`, `*_TOKEN*`, `*_KEY*`, and `*_PASSWORD*` environment variables scrubbed. Instead, Nexus provisions executable (`0755`) wrappers in `<state_dir>/worker-bin/` and a dedicated `<state_dir>/worker-gitconfig`:

- **`gh-agent`**: Wraps `gh`, injecting the configured GitHub bot token or `GH_CONFIG_DIR` only inside the `gh` child process.
- **`lark-bot`**: Wraps `lark-cli`, binding the configured Feishu profile (`--profile <profile>`).
- **`gws-agent`**: Wraps `gws` for Google Workspace operations.
- **`dws-agent`**: Wraps `dws` for DingTalk Workspace operations.
- **`nexus-worker`**: Wraps `nexus worker ...` callbacks (`report`, `read`, `handback`, `switch-target`).
- **`worker-gitconfig`**: Rewrites `git@github.com:` SSH URLs to `https://github.com/` and delegates git authentication to `!gh-agent auth git-credential`.

---

## 3. Durable Effects & Outbox Delivery (`src/infra/effects.rs`)

Side effects queued in `EngineState.effects` (`RunEval`, `LaunchAssignment`, `DeliverInputs`, `SendOutbox`) are drained by the runtime control loop with exponential backoff (`min(1000 * 2^attempts, 60_000)` ms, max 5 attempts). Outbound messages (`OutboxMessage`) enforce the 100-word limit on IM channels and are delivered in causal order per conversation.

---

## 4. Dual-Layer Watchdog (`src/infra/watchdog.rs`)

The Watchdog monitors active Worker assignments across two dimensions:
1. **System Health**: Detects crashed subprocesses or stalled heartbeats.
2. **Semantic Health via `Jev`**: Feeds the Worker's recent command/output trace into `Jev` (`is_looping` and `is_drifting` boolean questions). When `p >= 0.85`, the Watchdog automatically escalates `ThinkEffort` (`ThinkEffort::escalate()`) and invokes `Engine::switch_execution_target` with a `HandoffCard` so the retried Assignment starts with higher reasoning capacity and explicit awareness of the prior failure.
