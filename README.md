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
- 连接配置持久化：`~/.goldagent/connect.json`
- Skill 技能加载：`~/.goldagent/skills/*/SKILL.md`

## 前置要求

- Rust 工具链（`cargo`、`rustc`）
- 推荐在对话里用 `/connect` 进行连接切换（会持久化到 `connect.json`）
- 也支持传统环境变量：`OPENAI_API_KEY`

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

# 对话内命令面板（输入 /）
# 会展示可用命令，并提示 skill

# 连接后端
cargo run -- connect status
cargo run -- connect login --model gpt-5
cargo run -- connect api sk-xxxx --model gpt-4.1-mini
cargo run -- connect api sk-ant-xxxx --provider anthropic --model claude-3-7-sonnet-latest
cargo run -- connect api sk-xxxx --provider zhipu --model glm-5
# 推荐在对话里走统一流程：/connect <provider> -> 选择 api/login

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

## 对话内 Slash 命令

在 `cargo run` 的对话窗口中可直接输入：

- `/` 或 `/help`：显示命令面板
- `/model`：查看模型状态
- `/model <model>`：切换模型（`/model` 后可上下选择候选模型）
- `/connect`：进入连接设置
- `/connect status`：查看连接状态（厂商/模式/模型/账户/用量）
- `/connect openai`：进入 OpenAI 连接方式（`login` / `api`）
- `/connect anthropic`：进入 Anthropic 连接方式（`api`）
- `/connect zhipu`：进入智谱 GLM 连接方式（`api`）
- `/connect <provider> api`：进入 API Key 交互输入流程（统一）
- `/connect <provider> api <KEY> [model]`：直接切换为 API Key 模式（统一）
- `/connect openai login [model]`：切换为 OpenAI 登录态
- `/skill`：进入 skill 选择
- `/skill <skill名> <输入>`：运行 skill
- 当只输入 `/skill <前缀>` 时，会提示匹配的 skill 名称
- `/clear`：清屏并重绘窗口
- `/exit`：退出对话

命令面板支持键盘操作：

- `↑/↓`：上下选择候选项
- `Tab` 或 `Enter`：补全当前选中命令（补全后再次回车执行）

## Cron 表达式说明

- 支持 5 段格式：`分 时 日 月 周`
  - 示例：`0 9 * * 1-5` 表示工作日 9:00
- 也支持 6 段（含秒）：
  - `0 */15 * * * *`

## 数据目录

GoldAgent 运行数据默认写入：

`~/.goldagent/`

也可通过环境变量覆盖：

`GOLDAGENT_HOME=/path/to/your/dir`

- `MEMORY.md`：长期记忆
- `memory/YYYY-MM-DD.md`：短期过程日志（按天）
- `jobs.json`：定时任务配置
- `connect.json`：连接方式配置（登录态 / API）
- `usage.json`：本地用量统计（请求数、输入/输出 tokens）
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
