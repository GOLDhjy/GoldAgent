# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
cargo build                   # compile
cargo run                     # start interactive chat loop
cargo run -- init             # initialize ~/.goldagent/ data directories
cargo run -- run "<task>"     # single-turn task
cargo test                    # run all tests
cargo test <test_name>        # run a single test (e.g. cargo test promotes_repeated)
cargo fmt --all               # format code
cargo clippy --all-targets -- -D warnings  # lint (must pass before PR)
```

Run `cargo fmt` and `cargo clippy` before opening a PR.

## Architecture

This is a Rust (edition 2024) async CLI application. The entry point is `src/main.rs`, which parses CLI args via `clap` and routes to one of the top-level commands or falls through to the default `chat_loop`.

### Module Overview

| Module | Responsibility |
|---|---|
| `cli.rs` | `clap`-based argument definitions (`Commands`, `CronCommand`, `HookCommand`, `SkillCommand`, `ConnectCommand`) |
| `main.rs` | Command router, `chat_loop` (interactive REPL with raw-mode input), slash command handling, system prompt construction |
| `provider.rs` | `ProviderClient`: multi-provider HTTP chat client (OpenAI/Codex login, OpenAI API, Anthropic, ZhiPu). Handles model selection, hint items for `/model`, and `/connect` chat commands |
| `connect.rs` | `ConnectMode` / `ConnectProvider` enums; reads and writes `~/.goldagent/connect.json` |
| `memory.rs` | Long-term memory (`MEMORY.md`) and short-term daily memory (`memory/YYYY-MM-DD.md`). Handles auto-promotion logic (repeated sentences → long-term), explicit "remember this" capture, and capability/connect-rule declarations |
| `chat_actions.rs` | Parses `[[LOCAL_ACTION:{...}]]` control lines emitted by the LLM response and dispatches them to cron/hook operations |
| `jobs.rs` | Cron job CRUD; persists to `~/.goldagent/jobs.json` |
| `hooks.rs` | Git/P4 hook CRUD; persists to `~/.goldagent/hooks.json` |
| `scheduler.rs` | `serve` command — runs cron job executor and hook pollers concurrently |
| `daemon.rs` | Auto-starts or reloads the `serve` background process when a job/hook is added |
| `skills.rs` | Loads `~/.goldagent/skills/*/SKILL.md` skill definitions; `create_skill` scaffolds a new skill; `run_skill` calls the provider with the skill's system prompt |
| `config.rs` | `AgentPaths` — single struct that resolves all runtime paths (respects `GOLDAGENT_HOME` env var) |
| `shell.rs` | Safe shell command execution with a "dangerous command" check; `--force` flag bypasses the check |
| `notify.rs` | System notification for the `remind` command |
| `usage.rs` | Tracks request count and token usage in `~/.goldagent/usage.json` |

### Key Data Flow

1. **Chat loop**: `main.rs:chat_loop` → builds system prompt with memory context → sends to `ProviderClient::chat` → parses `LOCAL_ACTION` from response → executes cron/hook action or prints text → appends to short-term memory → auto-promotes to long-term memory.

2. **LOCAL_ACTION protocol**: The LLM can emit `[[LOCAL_ACTION:{...}]]` at the start of a response. `chat_actions.rs` extracts and executes these to add/list/remove cron or hook jobs without the user running CLI commands manually.

3. **Multi-provider**: `connect.json` stores the active backend. `ProviderClient::from_paths` reads it, falling back to `OPENAI_API_KEY` env var. Providers: OpenAI login (via `codex` CLI subprocess), OpenAI API, Anthropic API, ZhiPu (general / coding endpoints).

4. **Scheduler**: `goldagent serve` runs the cron executor (`jobs.rs` schedule matching) and hook pollers (git commit-hash or P4 counter polling) as concurrent tokio tasks. Adding a job/hook auto-starts or reloads the daemon via `daemon.rs`.

### Runtime Data (`~/.goldagent/` or `$GOLDAGENT_HOME`)

- `MEMORY.md` — long-term memory (append-only sections)
- `memory/YYYY-MM-DD.md` — short-term daily log
- `jobs.json` — cron job definitions
- `hooks.json` — git/p4 hook definitions
- `connect.json` — active provider connection
- `usage.json` — token usage counters
- `skills/*/SKILL.md` — installed skill definitions

### Planned Feature

`supervise-plan.md` describes a planned `goldagent supervise` command (not yet implemented). It will live in a new `src/supervise.rs` module and supervise external agents via a GA_STATUS/GA_EVIDENCE log-marker protocol.

## Testing

Tests are colocated with implementation using `#[cfg(test)] mod tests`. The main test coverage is in `src/memory.rs`. Name tests by behavior (e.g., `promotes_repeated_sentence_to_long_term`). Use isolated temp paths for any filesystem-touching tests — do not write to the real `~/.goldagent/` tree.

## Commit Style

Follow `feat: ...` / `fix: ...` / `refactor: ...` prefixes. Keep commits to one logical change.
