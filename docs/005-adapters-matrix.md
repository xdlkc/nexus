# 005 — Peripheral Adapters Matrix (`lark-cli` + `github` & Workspace Ecosystem)

## 1. Adapter Contracts (`src/adapters/traits.rs`)

Nexus decouples environmental I/O from its core domain state machine via two traits:

- **`SourceAdapter`**: Polls or receives events from an external system (`Feishu`, `Github`, `Gws`, `Dingtalk`, `Telegram`, `Slack`, `Wechat`), returning:
  - `triggers: Vec<Trigger>` (normalized with `thread_key`, `reply_channel`, and parsed `[harness:provider/model:think]` directives)
  - `closed_threads: Vec<(String, String)>` (e.g. `("github:pr:owner/repo#42", "merged")` so `Engine::thread_closed` automatically completes waiting `Ticket`s)
  - `next_cursor: Option<String>` (persisted atomically in `SqliteStore::set_cursor`)
- **`ChannelAdapter`**: Delivers `OutboxMessage` payloads to a `ChannelAddress` (`FeishuTopic`, `GithubIssueComment`, `GithubReviewReply`, `GwsGmailDraft`, `GenericIm`).

---

## 2. Primary Production Pipeline: Feishu (`lark-cli`) + GitHub (`gh`)

### 2.1 Feishu Adapter (`src/adapters/feishu.rs`)
- **Inbound Polling (`lark-cli` / `lark-bot`)**:
  - Invokes `lark-bot api GET "/open-apis/im/v1/messages?container_id_type=chat&container_id=<chat_id>&page_size=20"` (or `lark-cli im message list`).
  - Parses text/post JSON content (`{"text": "..."}`), resolves `@bot` mentions (`mentions` array or `@nexus` prefix), extracts inline `[harness:provider/model:think]` overrides via `parse_inline_target_directive`, and maps root/parent message IDs into topic thread key `feishu:topic:<chat_id>:<root_message_id>`.
  - Populates `ChannelAddress::FeishuTopic { source_id, chat_id, root_message_id, reply_to_message_id }`.
- **Group Member Allowlist Sync**:
  - Invokes `lark-bot api GET "/open-apis/im/v1/chats/<chat_id>/members"` to populate `SourceAdmissionPolicy.allowed_actors` with group member `open_id`s.
- **Outbound Topic Reply**:
  - Invokes `lark-bot api POST "/open-apis/im/v1/messages/<reply_to_message_id>/reply"` with `{"msg_type":"text","content":"{\"text\":\"...\"}","reply_in_thread":true}`.

### 2.2 GitHub Multi-Repo Adapter (`src/adapters/github.rs`)
- **Inbound Polling (`gh` / `gh-agent`)**:
  - Polls repository Issue/PR comments (`gh-agent api "repos/<owner>/<repo>/issues/comments?since=<cursor>&per_page=30"`) and PR review comments (`gh-agent api "repos/<owner>/<repo>/pulls/comments?since=<cursor>&per_page=30"`).
  - Normalizes PR/Issue threads to `github:pr:<owner>/<repo>#<number>` or `github:issue:<owner>/<repo>#<number>`.
  - Polls open PR states (`gh-agent pr view <number> --repo <owner>/<repo> --json state,mergedAt`) for any `FollowedThread` (`github:pr:<owner>/<repo>#<number>`) so merged or closed PRs emit `closed_threads` and auto-complete their bound `Ticket`.
- **Outbound Delivery**:
  - `ChannelAddress::GithubIssueComment`: `gh-agent api "repos/<repo>/issues/<number>/comments" -f body="<text>"`
  - `ChannelAddress::GithubReviewReply`: `gh-agent api "repos/<repo>/pulls/<pull_number>/comments/<comment_id>/replies" -f body="<text>"`

---

## 3. Extended Ecosystem Adapters
- **Google Workspace (`src/adapters/gws.rs`)**: Wraps `gws-agent` (`gws gmail`, `gws calendar`, `gws drive`) for unread email/calendar triggers and Gmail draft replies (`ChannelAddress::GwsGmailDraft`).
- **Additional IM Channels (`src/adapters/im_channels.rs`)**: Unified `DingtalkAdapter` (`dws-agent`), `TelegramAdapter`, `SlackAdapter`, and `WechatAdapter` conforming to `SourceAdapter` and `ChannelAdapter`.
