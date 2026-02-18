# Repository Guidelines

## Project Structure & Module Organization
This repository is a Rust CLI application. Core code lives in `src/`, with `src/main.rs` as the command router and chat loop entry point. Feature modules are split by concern: `cli.rs` (argument parsing), `provider.rs` and `connect.rs` (model/provider integration), `memory.rs` (long/short memory), `jobs.rs` + `scheduler.rs` (cron execution), and supporting modules like `skills.rs`, `config.rs`, `shell.rs`, and `usage.rs`.

Keep runtime data out of Git: GoldAgent writes state under `~/.goldagent/` (or `GOLDAGENT_HOME` if overridden).

## Build, Test, and Development Commands
- `cargo build`: compile the project.
- `cargo run -- init`: initialize local GoldAgent data directories.
- `cargo run`: start the interactive chat loop locally.
- `cargo run -- run "<task>"`: execute a single-turn task.
- `cargo test`: run all unit tests.
- `cargo fmt --all`: format code using Rustfmt.
- `cargo clippy --all-targets -- -D warnings`: lint and fail on warnings.

Run `cargo fmt` and `cargo clippy` before opening a PR.

## Coding Style & Naming Conventions
Use Rust 2024 idioms and standard Rust formatting (4-space indentation, no tabs). Follow naming conventions used in `src/`: modules/functions in `snake_case`, types/enums in `PascalCase`, constants in `SCREAMING_SNAKE_CASE`.

Prefer small, single-purpose functions and explicit error propagation with `anyhow::Result` for command flows. Keep command parsing in `cli.rs` and avoid mixing UI printing, persistence, and provider logic in one function.

## Testing Guidelines
Tests are currently unit-style and colocated with implementation (`#[cfg(test)] mod tests`, see `src/memory.rs`). Name tests by behavior (e.g., `promotes_repeated_sentence_to_long_term`).

When adding features, include or update focused unit tests and run `cargo test`. For filesystem behavior, use isolated temp paths and avoid writing to a real `~/.goldagent` tree.

## Commit & Pull Request Guidelines
Recent history follows concise, typed subjects like `feat: ...` and `Refactor ...`. Prefer:
- `feat: ...` for new behavior
- `fix: ...` for bug fixes
- `refactor: ...` for internal restructuring

Keep commits scoped to one logical change. PRs should include a short problem statement, key implementation notes, and verification steps (commands run, e.g., `cargo test`, `cargo clippy`). Include sample CLI output when behavior changes are user-visible.

## Security & Configuration Tips
Never commit API keys, provider tokens, or generated files from `~/.goldagent/`. Use environment variables (`OPENAI_API_KEY`, `GOLDAGENT_HOME`) for local configuration and testing.
