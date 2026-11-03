# 003 — Polymorphic Worker Pool & Multi-Harness Drivers (`src/workers/`)

## 1. Priority-Aware Concurrency (`WorkerSlots`)

A single resident Router orchestrates $N$ concurrent Worker assignments (`max_concurrent_workers`, default `4`). To prevent long-running background tickets from starving interactive human follow-ups:

- **General Launches** (`NewTicket` or `Observe -> Auto` proactive tasks) may consume up to $N - 1$ slots when $N \ge 2$.
- **Human Wakeups** (resuming or delivering a human message to a subscribed `Ticket`) may use all $N$ slots, guaranteeing that human replies in Feishu or GitHub PRs never queue behind speculative background tasks.

---

## 2. Multi-Repo `git worktree` Lifecycle

Every `Ticket` owns a dedicated directory `<worktree_root>/<short_name>` on branch `agent/<short_name>`:

1. **`prepare_worktree(repo_checkout, worktree_dir, branch_name)`**:
   - Runs `git worktree add -B agent/<short_name> <worktree_dir> HEAD` if the worktree does not yet exist.
   - If the `Ticket` is switching Harnesses mid-flight (`Assignment #1 -> Assignment #2`), the existing worktree and all uncommitted/committed changes are preserved intact.
2. **`capture_git_status_diff(worktree_dir)`**:
   - Captures `git status --short` + `git diff --stat` when switching `(HarnessKind, provider, model, ThinkEffort)` so the incoming Harness receives an exact snapshot inside its `HandoffCard`.

---

## 3. Unified `WorkerHarnessDriver` & Concrete Drivers

All 5 Worker Harnesses implement a single `WorkerHarnessDriver` protocol and receive:
- `PATH` prepended with `<state_dir>/worker-bin` (`gh-agent`, `lark-bot`, `dws-agent`, `gws-agent`, `nexus-worker`)
- `GIT_CONFIG_GLOBAL=<state_dir>/worker-gitconfig`
- `NEXUS_TICKET_ID`, `NEXUS_ASSIGNMENT_ID`, `NEXUS_API_URL`
- Sensitive host credentials (`*_SECRET*`, `*_TOKEN*`, `*_KEY*`, `*_PASSWORD*`) scrubbed from subprocess environments when running external CLI agents.

| `HarnessKind` | Driver Implementation | CLI / Protocol Invocation | `ThinkEffort` Mapping |
| :--- | :--- | :--- | :--- |
| `pi-native` | `PiNativeDriver` | In-process Rust ReAct loop (`read_file`, `write_file`, `edit_file`, `run_command` with 5s auto-backgrounding, `job_output`, `kill_job`, `orchestrator_read`, `worker_report`) | **Phase-Aware**: starts at `ticket.think_effort` (`Planning`); auto-downgrades by 1 tier after first file edit (`Implementation`) |
| `pi-rpc` | `PiRpcDriver` | `pi --mode rpc --provider <p> --model <m>` (JSONL over `stdin`/`stdout`) | `{"type":"set_thinking_level","level":"<off\|minimal\|low\|medium\|high\|xhigh>"}` |
| `claude-code` | `ClaudeCodeDriver` | `claude -p --verbose --output-format stream-json --permission-mode bypassPermissions --model <m> --effort <low\|medium\|high\|max>` | `Minimal/Low -> low`, `Medium -> medium`, `High -> high`, `Xhigh -> max` |
| `codex` | `CodexDriver` | `codex exec --json --dangerously-bypass-approvals-and-sandbox -m <m> -c model_reasoning_effort="<minimal\|low\|medium\|high\|xhigh>"` | Maps directly to `model_reasoning_effort` |
| `opencode` | `OpencodeDriver` | `opencode run --format json --model <provider>/<model>` | Passed via `--variant` / prompt header |
