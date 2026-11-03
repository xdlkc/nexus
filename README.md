# Nexus

**事件驱动、双核路由的自主 Super-Agent 系统底座（Harness）**

Nexus 是一个持续在线、跨渠道协同的 Super-agent 运行时系统。它将即时通讯（飞书、钉钉、Telegram、Slack、微信）、代码托管平台（GitHub）与办公套件（Google Workspace）接入统一的事件流，在后台并发调度多种 Agent 执行器完成任务，让人的角色从繁琐的“单会话盯梢与上下文搬运”上移到更高层的 **“决策与验收”**。

---

## 核心思想

主流 Agent 工具默认将“任务”绑定在彼此隔离的单次对话窗口（`Session`）中：单任务的上限取决于模型能力，而多任务并发的上限却受制于人的注意力。Nexus 旨在打破这一限制，将 `Session` 退化为系统底层的实现细节：

- **最小化注意力消耗**：系统的每一条通知、每一次提问都在消耗人的注意力。Nexus 仅在结果就绪、需要关键决策或任务受阻时主动发声，其余时间在后台静默推进。
- **跨信道全局连续**：无论你在飞书/微信群里讨论方案、在 GitHub PR 下回复评审，还是收到一封工作邮件，面对的始终是同一个拥有全局连续记忆的 Agent。
- **确定性守门 + 概率性决策 + 生成式编排**：融合经典计算机系统的分层思想，用确定性规则与轻量判别模型过滤环境噪音，只把高价值决策交给生成式大模型。
- **工单与执行器解耦**：工作单元（Ticket）与具体执行方式分离，支持按任务特性自由组合并随时切换底层的执行 Harness、模型与思考强度（Think Effort）。

---

## 系统架构

Nexus 采用 **双层解耦架构（常驻 Router 层 + 隔离 Worker 层）**，配合事件总线、持久化状态存储与外部自愈回路运行：

```mermaid
flowchart TB
  subgraph Peripherals["周边工作环境 (Sources & Channels)"]
    IM["即时通讯<br/>飞书 · 钉钉 · Telegram · Slack · 微信"]
    GH["代码协作<br/>GitHub (Issues / PRs / Reviews / CI)"]
    GWS["办公套件<br/>Google Workspace (Gmail / Calendar / Drive / Docs)"]
  end

  subgraph Intake["事件接入与多级过滤 (Event Intake)"]
    Gate["确定性准入 (Fast Path)<br/>白名单 · 范围规则 · 幂等去重"]
    FanOut["Thread 订阅直通 (Fan-out)<br/>已关联话题直接投递至对应 Worker"]
    Triage["判别式决策模型 (System-1 / Jev)<br/>防注入检测 · 主动规则匹配 · 任务定级"]
  end

  subgraph Brain["常驻总控层 (Router Layer)"]
    Router["全局 Router (System-2)<br/>事件批处理 (Batching) · 链式上下文召回<br/>跨任务协调 · 多信道统一回复"]
  end

  subgraph Hands["多态执行层 (Polymorphic Worker Pool)"]
    Dispatcher["工单调度器 (Ticket & Assignment)<br/>动态匹配并支持中途热切换：<br/>Harness × Model × Think Effort"]
    Workers["隔离执行环境 (Worktrees & Sandboxes)<br/>内置 Pi 异步引擎 · Claude Code · Codex · OpenCode"]
  end

  subgraph Foundation["基础设施与自愈 (Persistence & Watchdog)"]
    Store[("持久化状态库<br/>事务状态 · 异步队列 · 审计日志")]
    Watchdog["外部 Watchdog 回路<br/>存活监控 · 语义死循环检测 · 自动升档恢复"]
  end

  Peripherals -->|环境事件 (Triggers)| Gate
  Gate -->|已订阅话题| FanOut
  Gate -->|候选新事件| Triage
  Triage -->|高置信度任务| Dispatcher
  Triage -->|复杂协调 / 模糊请求| Router
  Router -->|分派工单| Dispatcher
  FanOut -->|上下文直通| Dispatcher
  Dispatcher --> Workers
  Workers -->|结构化汇报 / 待决提问| Router
  Router -->|适配各信道习惯输出| Peripherals
  Router & Dispatcher <--> Store
  Watchdog -.->|巡检与自愈| Router & Dispatcher
```

### 关键机制

1. **三级事件过滤与双核路由（Dual-Engine Router）**
   - **第一级（确定性快路径）**：通过身份白名单与群组策略毫秒级滤除无关噪音；对已绑定工单的话题（`Thread`）直接旁路直通对应 Worker，无需中心节点中转。
   - **第二级（System-1 判别模型 `Jev`）**：利用判别式分类模型对候选事件做单次并发评估——识别提示词注入风险、匹配团队主动规则、评估任务复杂度并推荐执行配置。
   - **第三级（System-2 生成式 Router）**：负责跨任务协调、按需多跳召回历史上下文（**Inline & Spill + 链式召回**），并在事件密集涌入时自动合并批处理（**Auto-Batching**）。

2. **多态 Worker 与跨执行器热接力**
   - 每个工单在独立的工作目录（如 `git worktree`）与凭证隔离环境中运行。
   - 支持为不同工单（或同一工单的不同阶段）指定不同的 **`Harness + Model + Think Effort`** 组合：从轻量日常事务、快速代码修改，到调用重型推理模型与外部专业 Coding Harness 进行深度架构重构。
   - 支持全双工异步执行：长耗时命令自动转入后台运行，不阻塞前台对话与中途插话。

3. **投机执行（Speculative Execution）与多信道适配**
   - 支持观察模式（`observe`）与自动模式（`auto`）：Agent 可根据群聊报错或邮件邀约在后台静默复现问题、核对日程或准备草稿，确认有价值后再交付人类验收。
   - 输出自动匹配信道带宽：在飞书/钉钉/微信等 IM 中保持简短低打扰，在 GitHub PR 与文档中输出完整结构化报告。

4. **持久在线与外部 Watchdog**
   - 所有事件、工单状态与待发消息均经事务持久化，进程重启后可从断点恢复。
   - 独立的外部 Watchdog 不仅监控进程与接口存活，还从语义层面检测 Worker 是否陷入死循环或偏离目标，并自动介入修复或升档重试。

---

## 支持的生态组件

- **即时通讯（IM）**：飞书（`lark-cli`）、钉钉（`dws`）、Telegram、Slack、微信（WeCom / 网关桥接）
- **代码与研发平台**：GitHub（`gh` 多仓库 Issue、Pull Request、Code Review、CI 状态）
- **办公与个人效率**：Google Workspace（`gws`：Gmail、Calendar、Drive、Docs、Sheets）
- **Worker 执行引擎（Harnesses）**：内置原生异步引擎、Pi（`pi --mode rpc`）、Claude Code、Codex、OpenCode

---

## 安装与快速上手

### 环境要求
- **编译与运行时**：Rust 1.85+（Edition 2024，编译后为单一无依赖静态二进制）、`git` 2.32+、`tmux`
- **按需安装的外部 CLI**：
  - GitHub 协作：`gh` (GitHub CLI 2.48+)
  - 飞书接入：`lark-cli`
  - 钉钉接入：`dws`
  - Google Workspace 接入：`gws`
  - 外部 Worker Harness（可选）：`pi`、`claude`、`codex`、`opencode`

### 编译与安装

```bash
git clone <repository-url> nexus
cd nexus
cargo build --release
```

### 启动与管理

```bash
# 初始化配置与凭证隔离环境
nexus setup

# 启动常驻服务（后台守护模式）
nexus start

# 查看运行状态与健康检查
nexus status

# 平滑停止服务
nexus stop
```

服务启动后，可通过本地管理面板查看当前需要人工决策的工单（`Awaiting Answer` / `Awaiting Review`）、调整准入策略与配置执行器模型档位。

---

## 开发指南

```bash
# 开发模式运行
cargo run -- start --foreground

# 代码静态检查与格式检查
cargo clippy --all-targets -- -D warnings && cargo fmt --check

# 运行单元测试与仿真测试集
cargo test
```

### 贡献与扩展
- **新增外部事件源或通知渠道**：实现统一的 `Source` / `Channel` 协议插件并注册至事件总线。
- **接入新的 Worker 执行器**：实现标准的 `WorkerHarnessDriver` 接口，完成模型参数、思考强度与状态汇报协议的映射。

---

## 许可证

MIT
