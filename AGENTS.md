# Repository Guidelines

## Project Structure & Module Organization

RouteLane is a Rust 2021 Linux desktop application using GTK4/Libadwaita. `src/ui/` contains the desktop interface; `src/domain/` and `src/config/` define profiles and persistence. `src/backend/` implements planning, topology, nftables, and privileged transactions. `src/daemon/` serves user IPC; `src/application/` handles process affinity. `src/bin/` contains the daemon, helper, and CLI entry points.

Integration contracts live in `tests/*_contract.rs`, reusable evidence in `tests/fixtures/`, and opt-in kernel tests in `tests/root/`. Desktop/service assets are in `data/`; Debian packaging is in `packaging/deb/`. Read `docs/superpowers/specs/` and `docs/superpowers/plans/` for feature decisions.

## Build, Test, and Development Commands

Install Rust stable and GTK4/Libadwaita development packages first. In this workspace, prefix shell commands with `rtk`; use `rtk proxy` for unsupported commands.

- `rtk cargo build --release --bins`: build all four binaries.
- `rtk cargo run --bin routelane`: launch the desktop client; installed daemon/helper integration is separate.
- `rtk cargo test --all-targets`: run rootless unit and contract tests.
- `rtk proxy cargo fmt --all -- --check`: check formatting.
- `rtk cargo clippy --all-targets -- -D warnings`: reject lint warnings.
- `rtk proxy bash scripts/run-gtk-smoke.sh`: exercise GTK in an isolated session.
- `rtk proxy bash packaging/deb/build-deb.sh`: produce a development `.deb`, not install it.

## Coding Style & Naming Conventions

Use rustfmt defaults, four-space indentation, `snake_case` functions/modules, and `PascalCase` types. Keep privileged interfaces typed and narrowly scoped. Preserve compatible GTK/Libadwaita versions; justify new dependencies. Keep UI text consistent with `src/ui/i18n.rs`.

## Testing Guidelines

Use Rust `#[test]`/async tests and descriptive behavior names. Add regression coverage before changing routing, ownership, or rollback behavior. Run focused contracts, then the rootless suite. Compile kernel tests with `rtk cargo test --test root --features root-integration --no-run`; execute only in an authorized disposable VM. Exit 77 is **not verified**, never a pass. Compilation does not prove traffic or Polkit behavior.

## Commit & Pull Request Guidelines

Follow existing scoped messages: `feat(routing): ...`, `fix(nftables): ...`, `docs: ...`. Preserve unrelated dirty changes; stage only intended hunks. Describe scope, linked issues, verification, compatibility changes, and remaining risks. Include screenshots for UI changes.

## Security & Configuration

Never flush global firewall/conntrack state or rewrite foreign routes, DNS, or global tunables. Mutate only validated RouteLane-owned resources. User approval does not replace ownership proof. Never store sudo passwords or secrets. Ubuntu, Debian, and Fedora require separate release validation.
