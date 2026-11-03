# 002 双核路由与上下文管理 (Dual-Engine Router & Context Cache)

本文档基于 [001 领域模型](001-domain.md)，定义事件从进入系统到完成路由裁决的 **三级流水线（`Gate -> Jev -> Router`）**，以及将模型上下文窗口作为高速缓存管理的 **`Inline & Spill` 与链式召回（Chain of Recall）** 规范。

---

## 1. 三级过滤与路由流水线

真实工作流中的群聊、代码评审与邮件事件具有显著的“高噪音、低触发率、不可信输入混杂”特征。Nexus 将路由决策拆分为三个递进层级：

### 1.1 Tier 0：确定性准入与订阅直通 (`Gate`)
在调用任何模型之前，由纯代码在 `< 1ms` 内完成确定性判断：
1. **幂等与防回环**：丢弃来自系统自身 Bot 账号的事件及已存在相同 `(sourceId, externalId)` 的重复事件。
2. **已订阅话题直通（Subscription Fan-out）**：
   - 若事件所属 `Thread` 已被一个或多个活跃 `Ticket` 订阅，该事件直接作为 `Input` 投递给对应 Worker。
   - 若事件为人类在 `Ticket` 主订阅 `Thread` 上的跟进消息，则由该 `Ticket` 的当前 Worker 决定如何处理（若超出工单范围，Worker 调用 `handback` 退回给 `Eval`）。
3. **准入强度判定（Admission Strength）**：
   - **`explicit`（明确请求）**：白名单内成员直接 `@agent`、私聊 Agent，或在准入表中配置为 `explicit` 的范围。
   - **`inferred`（推断候选）**：白名单成员未 `@agent` 的群聊讨论、白名单内 Bot（如 CI / Dependabot）的通知，或准入表中配置为 `inferred` 的范围。
   - **`not_admitted`（直接忽略）**：当接单模式为 `off` 时的所有 `inferred` 事件，或不在白名单内的无关消息。
4. **行内执行目标解析（Inline Target Directive）**：
   - 支持在消息中通过 `[harness:model:think]`（或简写如 `[codex:xhigh]`、`[claude-code:opus]`）显式指定目标执行器配置，解析结果直接附着在候选事件上。

---

### 1.2 Tier 1：System-1 判别式决策模型 (`Jev Triage`)
对于通过 `Gate` 但未直接扇出给已有工单的候选事件批次，系统调用 `@earendil-works/pi-ai` 的 `models.classify()` 接口（默认使用 `typesafe/jev-latest` 或配置的 `ClassifierModel`），在单次前向推理中完成多维结构化裁决：

#### 判别问题集 (`ClassifierContext.questions`)
1. **`injection_risk` (`bool`) —— 提示词注入防火墙**：
   - 检查事件内容是否包含试图覆盖系统指令、窃取本地凭据或诱导越权操作的恶意载荷。
   - 由于 `ClassifierModel` 无法调用工具且只输出受限概率分布，外部恶意文本在此层被彻底物理隔离。当 `probability >= 0.80` 时，直接拦截并记录安全告警。
2. **`rule_<ruleId>` (`bool`) —— 主动规则并发评估**：
   - 将当前 `Policy` 中启用的所有主动规则（如“自动复现群内报障”“收到会议邀约自动核对日程并起草回复”）展开为并行的 `bool` 问题，一次性得出每条规则的匹配概率。
3. **`route_action` (`choice`) —— 路由动作判定**：
   - 候选项：`ignore`（无关噪音）、`new_ticket`（开启新工单）、`attach_ticket`（并入现有工单）、`stop_ticket`（叫停现有工单）、`needs_llm_router`（语义模糊、需多跳检索历史或需直接文字回复）。
4. **`target_ticket` (`choice`，当存在活跃关联工单时)**：
   - 在当前关联的活跃工单 ID 列表中选择最匹配的目标工单。
5. **`task_kind` (`choice`) 与 `complexity` (`score`) —— 执行器自动选配**：
   - `task_kind`：`gws_workspace`（邮件/日程/文档）、`quick_code`（小型修改/解答）、`deep_refactor`（多文件重构/新特性）、`hard_debug`（疑难根因定位/并发死锁）。
   - `complexity`：五级打分 `["minimal", "low", "medium", "high", "xhigh"]`。

#### 基于概率阈值的分流规则
- **静默过滤**：当 `route_action == "ignore"` 且置信度 `>= 0.75`，且所有主动规则匹配概率 `< 0.30` 时，直接记录 `Decision(ignore)`，不唤醒生成式 Router。
- **高置信度快路径派单（Fast-Accept）**：当请求强度为 `explicit`（或处于 `auto` 模式且主动规则匹配概率 `>= 0.85`），且 `route_action` 为 `new_ticket` / `attach_ticket`（置信度 `>= 0.80`）时，直接根据 `(task_kind, complexity)` 映射表生成 `ExecutionTarget` 并派单。
- **升级至 System-2（Escalation）**：当置信度不足、命中 `needs_llm_router`、处于需生成确认文案的交互场景，或涉及跨多个工单的冲突协调时，将 `Jev` 的输出概率作为先验标签（Prior Hints）一并送入常驻生成式 Router。

---

### 1.3 Tier 2：System-2 常驻生成式路由会话 (`Nexus-Router`)
`Nexus-Router` 是一个常驻的轻量级生成式 Agent 会话，充当全局序点（Sequence Point）、跨任务记忆与唯一对外发言人：
- **原则上不直接执行重任务**：不挂载代码修改工具，且禁止执行超过 5 秒的阻塞等待命令。
- **自动批处理（Auto-Batching）**：当外部事件密集涌入而 Router 正处于上一轮推理中时，新到达的候选批次会自动在队列中合并，在下一次 LLM 调用中一次性注入，共享长上下文 Prefill 开销。
- **投机执行控制（`off | observe | auto`）**：
  - 在默认的 `observe` 模式下，命中主动规则的 `inferred` 事件会走完完整评估并记录为 `ignore (would_accept)` 及拟选定的 `ExecutionTarget`，供运维者在面板中审阅调优，确认无误后再切入 `auto` 模式静默预执行。

---

## 2. 上下文缓存管理：`Inline & Spill` 与链式召回

为了防止长期运行中上下文窗口膨胀稀释模型注意力，所有注入给 Router 和 Worker 的信息均遵循缓存局部性（Locality）原则：

### 2.1 `Inline & Spill` 预算规范
- **内联（Inline）**：
  - 当前 `Trigger` 的唯一 ID、发送者、时间戳及正文前 `1,200` 字符（若被截断则标注总字符数 `chars` 与读取 URI）。
  - 所属 `Thread` 的首尾与紧邻前后各 `5` 条消息摘要（每条限 `400` 字符）。
  - 紧密关联对象的唯一 ID（如 `replies_to`、同话题下的 `related_tickets` 前 8 条摘要卡片）。
- **溢出（Spill）**：
  - 超过长度预算的长消息、完整 Issue/PR 历史评论、代码 Diff 与 CI 构建日志均留在上下文窗口外，仅通过统一的 `orchestrator_read` 工具按需分页读取（默认每页 `4,000` 字符或 `20` 条记录，携带 `next_offset` 游标）。

### 2.2 链式召回 (`Chain of Recall`)
系统中的所有实体均拥有标准化的资源 URI，且每次读取返回的结果都自动内联下一跳关联实体的 URI，构成一张可按需遍历的引用图：

| 资源 URI 格式 | 返回内容与内联的下一跳引用 |
| :--- | :--- |
| `trigger/<id>` | 触发事件完整正文、被回复的 `trigger/<parentId>`、同话题相邻消息列表、所属 `thread/<key>` |
| `thread/<key>` | 话题元数据、关联的所有 `ticket/<id>` 卡片、近期 `decision/<id>` 记录及分页 `Trigger` 列表 |
| `ticket/<id>` | 工单状态、授权 `Mandate`、关联的所有 `thread/<key>`、当前与历史 `Assignments` 及未读 `Inputs` |
| `ticket/<id>/report/<assignmentId>` | 指定派工实例的完整汇报正文、提问详情与绑定的 PR 链接 |
| `decision/<id>` | 路由裁决原因码、`Jev` 判别概率快照及关联的 `trigger/<id>` 与 `ticket/<id>` |

Worker 会话在调用 `orchestrator_read` 时受到严格的权限边界限制：仅允许读取归属自身 `Ticket` 及其关联 `Thread` 的资源，防止不同工单之间的无关上下文串扰。
