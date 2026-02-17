# GoldAgent

GoldAgent 是一个使用 Rust 构建的本地 CLI 助手。

当前 MVP 已支持：

- 默认循环对话：直接执行 `goldagent`（开发阶段可用 `cargo run`）
- 显式对话命令：`goldagent chat`
- 单轮任务执行：`goldagent run "<任务>"`
- 全局文件记忆：`~/.goldagent/MEMORY.md`
- 短期记忆文件：`~/.goldagent/memory/YYYY-MM-DD.md`（按天）
- 自动长期记忆筛选：
  - 从对话中提取偏好/约束/目标
  - 用户明确说“记住这个/请记住”会立即写入长期记忆
  - 同一句内容高频出现（默认 >= 3 次）自动升级长期记忆
  - 新建 Skill / 新建 Cron 任务自动写入长期记忆
  - 会话接近历史压缩前会触发一次静默长期记忆提取
- Cron 任务持久化：`~/.goldagent/jobs.json`
- Shell 命令执行（含基础危险命令拦截）
- Skill 技能加载：`~/.goldagent/skills/*/SKILL.md`

## 前置要求

- Rust 工具链（`cargo`、`rustc`）
- 下面两种鉴权方式任选其一：
  - 设置 `OPENAI_API_KEY`
  - 使用 `codex login` 完成登录（无 API Key 也可运行）

## 快速开始

```bash
cargo run -- init
cargo run
```

如果你没有 API Key：

```bash
codex login
cargo run
```

## 常用命令

```bash
# 单轮任务
cargo run -- run "帮我总结今天工作并列出3个下一步"

# 循环对话（默认）
cargo run

# Shell
cargo run -- shell "ls -la"
cargo run -- shell "rm -rf /tmp/demo" --force

# Cron
cargo run -- cron add "0 9 * * 1-5" "goldagent run \"生成每日计划\""
cargo run -- cron list
cargo run -- cron remove <job_id>
cargo run -- serve

# Skill
cargo run -- skill list
cargo run -- skill new my-skill
cargo run -- skill run daily-summary "今天做了三件事：..."
```

## Cron 表达式说明

- 支持 5 段格式：`分 时 日 月 周`
  - 示例：`0 9 * * 1-5` 表示工作日 9:00
- 也支持 6 段（含秒）：
  - `0 */15 * * * *`

## 数据目录

GoldAgent 运行数据默认写入：

`~/.goldagent/`

- `MEMORY.md`：长期记忆
- `memory/YYYY-MM-DD.md`：短期过程日志（按天）
- `jobs.json`：定时任务配置
- `skills/*/SKILL.md`：技能定义文件

## Skill 模板建议

建议所有技能都基于统一模板创建，便于：

- 统一输入输出格式
- 明确执行步骤与约束
- 后续做自动校验与复用

可直接使用：

```bash
cargo run -- skill new my-skill
```
