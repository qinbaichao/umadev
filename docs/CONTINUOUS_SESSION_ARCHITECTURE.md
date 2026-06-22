# UmaDev 持续会话驱动架构(Continuous-Session Driving)

> 最佳实践设计 — 取代"每阶段单发"的底座驱动模型。
> 三个底座(claude-code / codex / opencode)统一为一个**持续会话 + 流式事件**抽象,
> 9 阶段治理流水线、确认门、审计、角色 critic、信任分级全部叠加在它之上。

## 1. 为什么现在的模型是错的

现状:每个阶段执行一次 `claude --print "<90KB 大 prompt>"`(codex `exec`、opencode `run` 同理)——
一个**全新、无状态、单发**子进程,跑完即退。后果(真机 dogfood 实测):

- **慢 10 倍**:每阶段冷启动 + 重喂全部上下文;docs 撑满 8 分钟预算被切、降级成离线骨架。
- **底座客套不干活**:单发模式下底座把每次调用当一轮"对话",回一句"要不要继续产出架构文档?"
  而不是自主写文件 —— 它在等确认,表现成"卡住"。
- **没真写代码**:单发 + 默认权限不放开 → 底座只"叙述"代码,工具循环没真跑,源码产出为零。
- **上下文不连贯**:9 个"陌生人"各做一段,前后不接。

**根因**:把底座当"文本函数"(prompt → 一坨文本)调用,而不是当它本来的样子——
一个**持续的 agentic 会话**,连续用工具干活。`umadev` 该退回去做编排 + 治理 + 设门,
而不是把活剁成 9 段单发。

## 2. 统一抽象:`BaseSession`

三个底座的持续模式形状完全一致,抽象成一个 trait(取代现有 `Runtime::complete` 单发语义):

```rust
/// 一个常驻的底座会话:整条 9 阶段 run 复用一个,上下文全程保留。
#[async_trait]
pub trait BaseSession: Send {
    /// 起会话:spawn 常驻底座进程 / server,开一个会话,放开权限到 trust 档位。
    async fn start(workspace: &Path, trust: TrustMode) -> io::Result<Self> where Self: Sized;

    /// 往同一会话注入一个阶段指令(命令式),返回该回合的流式事件。
    /// 上下文自动流动:底座记得前面 research / docs / 写过的代码。
    async fn send_turn(&mut self, directive: Turn) -> EventStream;

    /// 回应一次工具/权限请求(治理在线裁决:allow / deny / ask)。
    async fn respond(&mut self, req_id: ReqId, decision: Decision) -> io::Result<()>;

    async fn interrupt(&mut self) -> io::Result<()>;          // ESC / 超时中断当前回合
    async fn steer(&mut self, extra: String) -> io::Result<()>; // 运行中插队/纠偏
    fn fork(&self) -> io::Result<Self> where Self: Sized;     // 只读分叉:critic 团队
    async fn end(self) -> io::Result<()>;                    // 关会话
}

/// 流式事件 — 三家不同 wire 协议归一化成这一个枚举。
pub enum SessionEvent {
    TextDelta(String),                       // 助手输出增量(活着感)
    ToolCall { name: String, input: Value }, // 它在调工具:写文件 / 跑命令(治理 + 审计的落点)
    ToolResult { ok: bool, summary: String },
    NeedApproval { req_id: ReqId, action: String, target: String }, // 危险动作 → 门/确认
    TurnDone { status: TurnStatus },         // 本阶段回合结束(干完 / 失败 / 截断 / 中断)
}
```

**关键不变量**:`ToolCall` 是产出的真相(写了什么文件),不是 `TextDelta`(它说了什么)。
治理审计、硬门"真实代码产出"判定、TUI 工具行,全部以 `ToolCall`/文件系统为准。

## 3. 9 阶段流水线如何驱动它

```rust
let mut session = Base::start(workspace, trust)?;   // 整条 run 一个会话
for phase in plan.phases() {                        // research,docs,[门],spec,frontend,[门],backend,quality,delivery
    let mut events = session.send_turn(phase.directive());  // 命令式:"现在产出完整的三份文档,直接写,不要问我"
    while let Some(ev) = events.next().await {
        match ev {
            ToolCall { name, input } => {
                governance::check(&name, &input)?;   // PreToolUse:emoji/硬编码色/AI-slop 拦截(fail-open)
                audit::record_tool_call(&name, &input); // UD-EVID-002
                tui::show_tool(&name, &input);       // "正在写 src/App.tsx…"
            }
            TextDelta(t)        => tui::stream(t),
            NeedApproval { req_id, action, target } => {
                let d = trust.decide(&action, &target);  // auto 放行非危险;危险/门 问用户
                session.respond(req_id, d).await?;
            }
            TurnDone { status } => break,            // 本阶段干完
        }
    }
    // —— 节点治理 ——
    if phase.is_gate() { wait_for_user_confirm()?; }     // 文档确认门 / 预览确认门:停下问,确认后才发下一阶段
    if phase.is_last_code_phase() {                       // 硬门(已实现,git 无关)
        if plan_produces_code && source_files(workspace).is_empty() {
            return hard_stop("[fail] 未产出真实代码 — 流水线停止,未交付");  // 绝不伪装成功
        }
    }
}
```

**门 = 天然暂停点**:回合 `TurnDone` 后,UmaDev **不发下一条指令**,转而问用户;
进程/会话一直挂着、上下文都在,零成本等待。确认后再 `send_turn` 下一阶段。

**治理两条路**(并存):
- **常态** — 安装 `settings.json` 的 `PreToolUse` hook 调 `umadev hook`(claude);opencode/codex 走事件流里的 tool-call 审计 + ruleset。
- **关键节点** — 在线裁决(claude `can_use_tool` / codex `requestApproval` / opencode `permission.asked`),可改写入参(强制去 emoji/紫色)或打回。

## 4. 三底座各自实现(`umadev-host`)

### 4.1 claude-code — stream-json 双向 NDJSON
- 起:`claude --print --input-format stream-json --output-format stream-json --verbose --session-id <uuid> --permission-mode acceptEdits --allowedTools "Read,Edit,Write,Bash" --append-system-prompt "<阶段+治理约束>"`,stdin 常开。
- 注入:stdin 写一行 `{"type":"user","message":{"role":"user","content":"<命令式指令>"},"parent_tool_use_id":null,"session_id":""}` + 换行(**每行必须合法 JSON**,否则 claude `exit(1)`)。
- 观察:逐行读 stdout NDJSON,`system/init`(抓 session_id)→ `assistant`(含 tool_use)/`stream_event` → **`result`(回合完成,`stop_reason==end_turn` 才算干净)**。
- 治理:settings.json `PreToolUse` hook → `umadev hook`(返回 `permissionDecision: allow|deny`,fail-open=allow);或处理 `control_request{can_use_tool}` 回 `control_response{behavior}`。
- 中断:`control_request{interrupt}`。会话落盘 `~/.claude/projects/<cwd>/<id>.jsonl`,崩溃用 `--resume <uuid>` 恢复。
- 注意:用 `--append-system-prompt`(叠加),**不要** `--system-prompt`(整体替换会丢工具引导,退化成只会聊天)。

### 4.2 codex — `codex app-server`(JSON-RPC 2.0 / stdio)
- 起:`codex app-server`,`initialize` → `initialized` → `thread/start {cwd, sandbox:"workspaceWrite", approvalPolicy:"never"}`(拿 `thread.id`/`sessionId`)。
- 注入:`turn/start {threadId, input:[{type:"text",text:"<指令>"}]}`(同 thread = 上下文流动)。
- 观察:`item/started`→`item/completed`(commandExecution/fileChange = 真产出)、`turn/diff/updated`(累计 diff)、**`turn/completed`(回合完成)**。
- 门/治理:gate 阶段把 `approvalPolicy` 设非 `never`,收到 `item/*/requestApproval` 弹给用户,回 accept/decline。`turn/steer` 排队输入、`turn/interrupt` 中断、`thread/fork{ephemeral}` 做 critic。
- fallback 梯子:`app-server` → `codex mcp-server`(`codex`/`codex_reply` 工具)→ `codex exec --json` + `exec resume`(注意事件名是点分 `turn.completed`,与 app-server 的斜杠 `turn/completed` 是两套 schema,别混)。

### 4.3 opencode — `opencode serve`(HTTP + SSE)
- 起:`opencode serve --hostname 127.0.0.1 --port 0`(从 stdout `listening on http://…` 抓真实端口),env 注 `OPENCODE_SERVER_PASSWORD=<rand>`,所有请求带 `Authorization: Basic …` + `x-opencode-directory: <pct-encoded 项目路径>`。
- 开会话:`POST /session {permission:[{permission:"*",pattern:"*",action:"allow"}], agent:"build", model}`(一次 ruleset 预批准,工具静默放行,免逐事件往返)。
- 订阅:`GET /event?directory=<path>`(SSE,`{id,type,properties}`,贯穿全程)。
- 注入:`POST /session/:id/prompt_async {parts:[{type:"text",text:"<指令>"}]}`(立即 202,同 session 上下文保留)。
- 观察:SSE `message.part.updated`(`part.type=="tool"` → 工具/文件写入,`state: pending→running→completed`)、`text` → **`session.status.idle`(回合完成)**。
- 门/治理:停在 idle 处问用户;危险模式用细粒度 ruleset `ask` + `POST /permission/:id/reply`;`POST /session/:id/abort` 中断;`/session/:id/fork` 或多 session 做并行/critic。
- 注意:同 session **busy 时再发 prompt 会 `SessionBusyError`** —— 编排器必须串行(等 idle 再发);并行开多 session。

## 5. 它怎么修好真机暴露的每个问题

| 真机问题 | 持续会话怎么修 |
|---|---|
| docs 21-50min | 一个会话不重喂上下文 + 底座连续干活,接近"直接用底座"的速度 |
| 底座客套问"要不要继续" | 命令式指令 + 放开权限 + agentic 工具循环,它自主产出不等确认 |
| 没真写代码 | `acceptEdits`/`workspaceWrite`/`[*→allow]` + 观察 `ToolCall`/文件 = 它真写文件 |
| 空 run 伪装成功 | 硬门(已实现,数 `source_files()`)+ `ToolCall` 真相判定,没代码就判失败 |
| 看着卡死 | 流式事件实时进 TUI(工具行/文本/状态)+ 心跳;`TurnDone` 是确定的完成边界 |
| 日志冲花 TUI | 已修(日志写文件);流式事件是结构化的,不再刮 stdout 文本 |

## 6. 迁移计划(分阶段,每步可验证、可回退)

1. **抽象层**:在 `umadev-runtime` 定义 `BaseSession` trait + `SessionEvent`;保留旧 `Runtime::complete` 不动(共存,offline 仍用它)。
2. **claude 先行**:`umadev-host` 写 `ClaudeSession`(stream-json),单测覆盖 NDJSON 编解码 + result 边界 + hook 治理。用 `scripts/smoke` 真机验证一个最小 run 真出代码、几分钟内完成。
3. **runner 改造**:`umadev-agent` 的 9 阶段 runner 从"每阶段 complete"改为"一个 session + 每阶段 send_turn",门/审计/硬门/critic 接到事件流上。
4. **codex / opencode**:实现 `CodexSession`(app-server)、`OpenCodeSession`(serve)。`BACKEND_IDS` 三家齐。
5. **下线单发**:offline 兜底保留;三个真底座的 `--print`/`exec`/`run` 单发路径退役。
6. **全程不变量**:fail-open 治理、确定性控环(门/退出码,LLM 只 advisory)、不持有模型端点、审计证据、硬门。

## 7. 不变的护城河

持续会话只换"怎么驱动底座",**不动**这些:9 阶段 + 文档确认门 + 预览确认门 + 质量/红队 + 硬门 +
审计证据 + 角色 critic 团队 + 信任分级 + 自我进化记忆 + 三语 + fail-open。
治理依然在每次文件写时拦截,门依然停下问用户,只是底座从"9 个陌生人单发"变成"一个连续干活的会话"。
