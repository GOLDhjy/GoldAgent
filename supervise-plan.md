# GoldAgent 外部 Agent 监督执行与自动验收（V1）实施计划

## 简述
为 GoldAgent 增加一个专业“监督外部 agent 编程执行”能力：按计划循环驱动外部 agent，基于日志规则与测试结果自动验收，支持最大循环上限与超限人工确认，最终产出结构化报告，默认落盘到当前目录，便于后续 CI/CD 接入。

## 已锁定决策（按你的选择）
1. 入口采用独立 CLI 命令，不做聊天入口。
2. 外部 agent 接入方式是固定模板命令执行，不做每轮动态命令生成。
3. 完成判定要求“日志规则 + 测试都通过”。
4. 日志判定采用纯规则，不用 LLM。
5. 循环策略采用“每轮快测 + 最终全量测试”。
6. 日志协议采用强制状态标记，格式是单行 Key-Value。
7. 达到最大循环后，交互终端提供“继续 N 轮 / 标记通过 / 标记失败”。
8. 无交互终端（CI）达到上限时直接失败并输出报告。
9. 测试命令通过每次 CLI 参数传入。
10. 默认工作目录为当前目录，可通过参数覆盖。
11. 报告默认写到当前目录。

## 公开接口与命令变更
1. 在 `/Users/gold/Documents/GoldAgent/src/cli.rs` 新增命令：
`goldagent supervise --task "<任务>" --plan-file <path> --agent-cmd "<命令>" --max-loops <N> --test-fast "<cmd>" --test-full "<cmd>" [--cwd <dir>] [--report <path>] [--force] [--agent-timeout-sec <s>] [--test-timeout-sec <s>]`
2. 约束：`--task`、`--plan-file`、`--agent-cmd`、`--test-fast`、`--test-full` 必填；`--max-loops` 默认 `6`，最小 `1`。
3. 在 `/Users/gold/Documents/GoldAgent/src/main.rs` 增加 `Commands::Supervise` 分支，调用监督引擎。
4. V1 不新增 `/supervise` slash 命令，避免双入口复杂度。

## 外部 Agent 协议（必须）
1. 外部 agent 每轮日志必须输出以下 Key-Value 行之一：
`GA_STATUS=DONE`、`GA_STATUS=NEEDS_WORK`、`GA_STATUS=BLOCKED`
2. 外部 agent 每轮日志必须输出：
`GA_EVIDENCE=<简短证据说明>`
3. 解析规则：取本轮 stdout+stderr 中“最后一次出现”的 `GA_STATUS` 与 `GA_EVIDENCE`。
4. 若缺失 `GA_STATUS` 或值非法，判定本轮失败，原因 `missing_or_invalid_status_marker`。

## 环境变量注入约定（传给外部 agent）
1. `GA_TASK`：用户任务文本。
2. `GA_PLAN_FILE`：计划文件绝对路径。
3. `GA_LOOP_INDEX`：当前轮次（从 1 开始）。
4. `GA_MAX_LOOPS`：当前允许最大轮次。
5. `GA_WORKDIR`：执行目录。
6. `GA_PREV_FEEDBACK_FILE`：上一轮验收反馈文件路径（首轮为空文件）。
7. `GA_RUN_DIR`：本次监督运行目录。
8. `GA_ATTEMPT_DIR`：当前轮次目录。

## 执行与验收流程（决策完成版）
1. 初始化运行上下文，生成 `run_id`，创建运行目录 `./.goldagent-supervise/<run_id>/`。
2. 读取计划文件并做存在性校验；不存在则立即失败。
3. 每轮执行外部 agent 命令，收集退出码、stdout、stderr、耗时。
4. 解析日志标记并生成“日志验收结果”。
5. 执行快测命令（可多条，按给定顺序），任一失败则本轮失败。
6. 若日志为 `DONE` 且快测全通过，则触发全量测试命令。
7. 全量测试通过则任务完成；否则本轮失败并进入下一轮。
8. 若日志为 `NEEDS_WORK` 或 `BLOCKED`，直接进入下一轮并附带反馈。
9. 每轮都写 `attempt-<n>.json` 和 `feedback.md`，供下一轮消费。
10. 达到最大轮次仍未完成时：
11. 有 TTY：提示三选一：继续 N 轮 / 标记通过 / 标记失败。
12. 无 TTY：直接标记失败并返回非 0 退出码。
13. 收尾写总报告 JSON 到当前目录，默认文件名 `./goldagent-supervise-report-<run_id>.json`，并在终端打印摘要。

## 代码结构落点
1. 新增 `/Users/gold/Documents/GoldAgent/src/supervise.rs`，承载监督主流程与判定逻辑。
2. 在 `/Users/gold/Documents/GoldAgent/src/shell.rs` 增加“保留非零退出码也返回输出”的执行函数（不复用会直接 bail 的当前函数）。
3. 在 `/Users/gold/Documents/GoldAgent/src/config.rs` 增加可选路径辅助函数，用于生成运行目录与报告默认路径。
4. 在 `/Users/gold/Documents/GoldAgent/src/main.rs` 接入 supervise 命令调用和退出码处理。
5. 在 `/Users/gold/Documents/GoldAgent/README.md` 补充 supervise 用法、日志协议、报告格式、CI 行为。

## 数据结构与报告 Schema（核心字段）
1. `SuperviseRunReport`：`run_id`、`task`、`plan_file`、`agent_cmd`、`cwd`、`max_loops`、`final_status`、`exit_code`、`started_at`、`finished_at`、`attempts`、`manual_decision`、`report_path`。
2. `AttemptReport`：`index`、`agent_exit_code`、`agent_status_marker`、`agent_evidence`、`fast_tests_passed`、`full_test_executed`、`full_test_passed`、`decision`、`reasons`、`duration_ms`、`stdout_path`、`stderr_path`。
3. `ManualDecision`：`continue_n`、`mark_pass`、`mark_fail`，可带 `note`。
4. `final_status` 取值：`passed`、`failed`、`manually_passed`、`manually_failed`。

## 失败模式与防护
1. 计划文件不存在：直接失败。
2. 外部 agent 命令执行异常：记录并计入失败轮次。
3. 标记缺失或格式错误：记录为协议违规失败。
4. 快测或全量测试命令执行异常：按失败处理并记录 stderr。
5. 超时：按失败处理，记录 `timeout` 原因。
6. 命令危险检查沿用现有策略；如用户显式 `--force` 则按现有语义放行。

## 测试方案
1. 单元测试：日志标记解析（正常、重复、缺失、非法值）。
2. 单元测试：轮次判定状态机（DONE+快测+全量、NEEDS_WORK、BLOCKED、无标记）。
3. 集成测试：模拟外部 agent 脚本输出 `GA_STATUS=NEEDS_WORK` 后 `DONE`，验证循环收敛。
4. 集成测试：快测失败应阻断 DONE，必须继续循环。
5. 集成测试：达到上限在无 TTY 场景返回失败。
6. 集成测试：报告 JSON 字段完整性与路径落盘。
7. 回归测试：不影响现有 `run/chat/cron/skill/connect` 命令行为。

## 验收标准（Done Definition）
1. `goldagent supervise` 可按计划循环驱动外部 agent 并自动验收。
2. 完成判定严格满足“日志协议 + 测试通过”双条件。
3. 支持最大循环与人工确认分支，CI 场景可稳定失败返回。
4. 默认在当前目录生成 JSON 总报告，内容可复盘每轮结果。
5. 现有命令和测试不回归。

## 假设与默认值
1. 外部 agent 可被 shell 命令调用，并可输出 `GA_STATUS`/`GA_EVIDENCE`。
2. 测试命令由调用方提供且在目标项目可执行。
3. V1 单任务串行，不做并发多任务监督。
4. V1 仅 CLI 入口，不做 slash、WebHook、CI Hook 自动触发。
5. V1 不引入 LLM 验收，纯规则判定优先稳定性与可解释性。
