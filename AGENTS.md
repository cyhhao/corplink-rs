# Repository Guidelines

## Project Structure & Module Organization
- `src/` holds all Rust modules, with `client.rs` handling network flows, `api.rs` templating endpoints, and `wg.rs` bridging to the embedded WireGuard runner.
- `config/` contains sample JSON configurations; runtime cookie artifacts live under `target/`.
- `libwg/` packages the prebuilt C shim required by `wg.rs`; avoid modifying binaries unless regenerating the bridge.

## Build, Test, and Development Commands
- `cargo fmt` — format the codebase with the Rustfmt rules enforced during reviews.
- `cargo check` — fast static analysis; run after every functional change to catch compile issues.
- `cargo test` — execute unit/integration suites (none today, but command should pass cleanly).
- `RUST_LOG=debug cargo run -- config/xxx.json` — run the client with verbose logging for endpoint debugging.

## Coding Style & Naming Conventions
- Follow standard Rust formatting: 4-space indentation, snake_case for functions/modules, UpperCamelCase for types.
- API endpoints and constants stay `SCREAMING_SNAKE_CASE` inside `api.rs`.
- Prefer small helper functions (e.g., `prepare_vpn_endpoint`) to avoid repeated networking boilerplate.
- Never leave ad hoc `debug!` dumps in long-term commits; rely on concise info-level logs.

## Testing Guidelines
- No dedicated framework yet; rely on `cargo test` scaffolding when introducing logic-heavy modules.
- Add unit tests beside the module under test (e.g., `src/utils.rs`); integration tests belong in `tests/` if introduced.
- Name tests `test_<behavior>` for clarity, and keep them hermetic (no live network calls).

## Commit & Pull Request Guidelines
- Commits use imperative, descriptive titles (`Add interactive VPN selection`, `Ensure peer route on macOS`).
- Group related edits; avoid mixing formatting-only shuffles with functional patches.
- Pull requests should include: summary of user-impacting changes, testing evidence (`cargo check`, manual runs), and per-API callouts when behavior changes.
- Link to tracking issues where applicable and call out risk areas (e.g., certificate handling, route management).

## Deployment & Configuration Tips
- Keep personal secrets out of configs; runtime cookies are generated into `target/` and should not be committed.
- When targeting new platforms, update `api.rs` templates and device identifiers in `Config` to mirror captured traffic before running live.
