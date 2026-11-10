@AGENTS.md

# CLAUDE.md — RouteLane

Commands, code style, testing and commit rules live in `AGENTS.md` and are not repeated here. This file holds durable project rules that `AGENTS.md` lacks, plus the entry point into in-flight work. Sources: `docs/handoff.md` (10 Nov 2026) and `README.md`.

**Language:** talk to the user and report results in **Turkish**. This file stays in English.

## Local-only documents (may be missing in a fresh clone)

- `docs/*` is git-ignored except files already tracked (`docs/handoff.md`, `docs/reliability-audit.md`, `docs/primary-failover-verification.md`, `docs/selective-routing-verification.md`, `docs/superpowers/**`, `docs/research/**`). A new file under `docs/` needs `git add -f`. `docs/handoff.md` **is tracked** now.
- `.superpowers/sdd/<date>-<feature>/progress.md` is the most detailed decision/test log, but it is local.
- Helper files such as `baseline.patch`, `task-*-brief.md`, `task-*-tests.log`, `task*-feature.patch` and `vm-*.sh`/`vm-*.py` are also local.
- If these are absent, do not treat that as an error; continue from commits, diff and spec/plan. That is why the durable rules below are copied here.

## Reading order (when starting a task)

1. `docs/handoff.md`: the newest `## En son kayıt` section at the top is the latest state, decisions and next step.
2. In-flight work: `docs/superpowers/plans/2026-11-10-audit-fixes.md` (fixes for the 10 Nov audit). Next feature plans: usage Plan 2 (RouteLane-routed counters) and Plan 3 (live connections) from `docs/superpowers/specs/2026-11-09-usage-and-events-design.md`; completed: `docs/superpowers/plans/2026-11-10-usage-quota-events.md` (Plan 1) and `docs/superpowers/plans/2026-10-22-selective-routing.md`.
3. `docs/superpowers/specs/2026-10-14-primary-connection-failover-design.md` and its plan: the completed primary/failover feature and its security boundary.
4. `.superpowers/sdd/<plan>/progress.md`, if present: detailed per-plan decision/test ledger.
5. `docs/research/primary-failover-kernel.md` and `docs/primary-failover-verification.md`: kernel/nft ABI evidence and the distribution matrix.
6. `README.md`, `docs/reliability-audit.md`, `docs/ui-ux-verification.md`: architecture and earlier verification. Historical results are not current evidence.
7. `docs/commercial-roadmap.md` (local only, removed from Git history): commercial scope only; not a substitute for the technical spec.

**Plan checkboxes alone are not the source of truth.** Cross-check them against commits, `git diff` and test logs.

## Scope discipline

- A documentation, handoff or review request is **not authorization to implement**. Before touching source code, confirm that the current request is to continue implementation.
- A documentation request does not authorize fixing fmt/lint failures you find either; report them.
- Do not install, publish or apply intermediate builds as a product or to the user's live network. Plan approval is not permission to disable/re-enable live RouteLane, install on the host, or edit the user's live profile or lab rules.
- **Working preference:** failover and selective routing were done on `main`, inline, with no new worktree and no subagents. For the usage and audit plans the user explicitly chose `/sous-chef:fire` (staged Codex tickets, head-chef line-by-line review and local verification between stages, commits by the head chef). Ask when a new plan starts unless the user already chose. Never report an independent review as done when it was not.

## Architecture and security boundary

Details are in `README.md`. Preserve these boundaries when making changes:

```text
GTK UI / routelanectl  --(versioned user Unix socket, 0600)-->  routelane-daemon
routelane-daemon  --(system D-Bus + Polkit io.github.routelane.apply)-->  routelane-helper (root, D-Bus activated)
```

- The GUI and `routelanectl` are unprivileged clients and talk only to the daemon. Both use the same IPC client (`src/ipc/client.rs`).
- The helper accepts **typed plans**, not arbitrary commands. Before every mutation it validates limits, exact ownership, generation, plan hash and live kernel state. A failed replacement rolls back to the previous generation.
- RouteLane's reserved resources are listed below; never write outside them:
  - nft table `inet routelane`
  - routing tables `20000..20099`
  - rule priorities `10000..10099`
  - marks `0x524c0001..0x524c0064`
  - cgroup subtree `/sys/fs/cgroup/routelane.apps/*`
- Journal files live under `/run/routelane`. They survive a helper service restart but not a reboot. The inspect path never replays a journal; recovery happens only in an authorized mutating call.
- The helper has no enable/install target and is never started directly; it is activated via system D-Bus. With no Polkit agent the result is `NeedsAuthorization`; there is no silent privilege escalation. No sudo password is ever requested or stored.
- Connection Health proves only local conformance with the plan. It uses no external "what is my IP" service and does not show that a site loads.
- Domain rules use the resolver IP snapshot taken when the profile is applied; there is no DNS/TTL tracking. Application affinity covers every new connection from the process tree; in a browser, other tabs join the same lane too. Flatpak and Snap are outside the first release's scope.
- The config schema is v4 (`CURRENT_SCHEMA_VERSION`, `src/config.rs`). v3, v2 and legacy documents are migrated read-only on load. Every v4 profile requires `routing` with `unmatched_traffic` (`PreserveSystem` default, or `UsePrimary`); unknown fields are rejected. The kernel plan uses it: `PreserveSystem` leaves unmatched traffic unmarked on the OS route, `UsePrimary` sends it through the primary. Daemon IPC protocol is **4**, runtime/helper protocol **3**, helper journal **3**; schema-2 journals are cleanup-only. If the schema changes, update the `README.md` example as well.
- A GUI crash is not caught by `fmt`/`test`/`clippy` (e.g. a `RefCell` double-borrow inside a live GTK callback). Do a real launch and diagnose with `journalctl --user -g routelane --since "-10 min"`. For an isolated launch use `scripts/run-gtk-smoke.sh`.
- **First official release gate:** each of Ubuntu 22.04/24.04/26.04, Debian 12/13 and Fedora 43/44 must be green in CI and release validation on both x86_64 and aarch64. Coverage: namespaces, install, systemd user, D-Bus, Polkit, GTK and rollback/health. The `.deb` is only a development verification artifact.

## Dirty working tree: preserve the user's changes

- The working tree was clean on 5 Nov 2026 (all earlier dirty work committed by origin). New uncommitted work can again mix origins: run `rtk git status --short` and read the latest handoff record before staging, and commit only what the current task owns, one origin per commit.
- When a file mixes origins (often `src/backend/privileged.rs` or `src/daemon/service.rs`), stage it hunk by hunk only. Never commit, restore or check out the whole file, and never copy it from another file. Verify each staged state in an isolated tree exported with `git checkout-index -a --prefix=<dir>/`.
- No `git add .`, `git reset --hard`, `git clean`, or whole-file `checkout`/`restore`.
- `git diff` does not show the contents of untracked files (e.g. `src/backend/interface_settings.rs`); read those separately.

## Product invariants (never violate)

- At startup the OS default connection is the primary. If the user picks a different primary, warn first and get confirmation.
- When the primary goes down, switch only to backups the user approved. On failback apply a 5 s stability period; do not tear down existing owned backup flows; only new flows go to the primary.
- IPv4 and IPv6 decisions are separate. With no eligible exit, stay **fail-closed**; no silent escape to the OS default.
- Route/Direct/Block and application affinity are preserved. Config migrations (to V3 and V4) never enable networking on a read-only load; a semantic change is explained on first activation. `Direct` means "use the primary", not an OS bypass.
- Never rewrite the OS main routing table, DNS, foreign firewall/policy rules or global sysctl values. Never use global NAT, conntrack flush, disabling IPv6 or a proxy to make tests pass.
- Approval does not permit deleting an unknown resource. Nothing is cleaned up just because its name contains `routelane`. The root helper mutates only with verified ownership and narrow, typed contracts. `Inspect` is read-only.
- Sysctl access is fixed-purpose: only `src_valid_mark` on the selected interface is changed. There is no IPC that accepts an arbitrary sysctl path/value; `all`/`default` are never written. The service stays `ProtectKernelTunables=yes`; only a documented narrow exception may be added.
- The `rules_v2_<hash>` chain is a consistency seal, not cryptographic authority. Numeric-only interface names are ambiguous in nft dumps: reject them, do not hide that by normalizing.
- Open security risks are listed in `docs/handoff.md` §8: owned route deletion on link loss, lane-ID/conntrack leakage, grant identity filtering, and recovery when a guard exists but the journal is missing. Read that section before touching these areas.
- `gtk4 0.5` / `libadwaita 0.2` / `glib 0.16` are pinned together (see the `Cargo.toml` comment). Do not upgrade them.

## Evidence honesty

- Exit 77, a successful compile, or a permission error is **not GREEN**. Nor does it substitute for an expected RED.
- The test stdio path is not real system-bus/Polkit authorization; never carry that bypass into production.
- Do not present past session results (test counts, VM passes) as current success. Rerun the command before quoting a number.
- Fake-backend contract tests do not prove real kernel, sysctl or nft behavior. Report rootless and isolated-kernel evidence separately.
- Ubuntu, Debian and Fedora each need separate validation. A single Ubuntu test or producing a `.deb` does not satisfy that.

## Kernel/root tests and the VM boundary

- **Never run root tests on the workstation with sudo.** Run them only inside an approved disposable guest, as root, via `bash scripts/run-root-integration.sh` or a verified prebuilt runner.
- **No approved test VM currently exists.** The test VMs `routelane-test-u2404-20261014` and `routelane-test-u2404-20261109` were deleted (the latter on 10 Nov 2026 after the selective routing kernel and system-bus runs). Prebuilt-binary VM runs replicate the host's absolute `target/debug/routelane-helper` and `scripts/` paths inside the guest and verify sha256 there. Creating a new disposable `routelane-test-*` VM needs the user's explicit approval first; until then kernel/root evidence is NOT VERIFIED. All other Multipass VMs (`central-lab-01`, `master-01`, `master-02`, `slave-01`, `pits-t-master`, `pits-t-slave1`, …) are the user's and out of scope; do not touch them. VM approval does not extend to host installation or live network mutation.
- `tests/root/system_bus_acceptance.sh` installs the package inside the guest and refuses to run unless `ROUTELANE_DISPOSABLE_VM` equals the guest's hostname. The multipass snap cannot read `/tmp`; stage transfers under the workspace (e.g. `.superpowers/sdd/...`).
- **Build trap:** `cargo test --all-targets` overwrites the feature-built `target/debug/routelane-helper` with a binary built without `root-integration`. Build the feature helper serially with the root tests, transfer it to the guest immediately, and verify the artifact identity (hash/date).
- Copies in the guest may be stale; compare hashes before using them.

## Current state (changes often; verify every session)

- Primary/failover: all 12 plan tasks committed (through `55906e8`); evidence in `docs/primary-failover-verification.md`.
- Selective routing: all 10 tasks committed (`7a7c636` … `69928ac`, review fix `1bf11cc`); Ubuntu 24.04 x86_64 kernel and installed-package system-bus evidence in `docs/selective-routing-verification.md` (one intermittent strict-RPF over-count, unresolved).
- Usage Plan 1 (interface totals, quota, notifications, 30-day event history and their screens): committed `7d753c5` … `7a92986`. Plans 2 and 3 not started.
- Audit fixes plan (`docs/superpowers/plans/2026-11-10-audit-fixes.md`): Tasks 1–3 `656ce38`, Tasks 4–5 `b321835`; check its ledger `.superpowers/sdd/2026-11-10-audit-fixes/progress.md` for the rest. Tasks 9–10 need a newly approved disposable VM.
- Version 0.1.2 (`b04e421`). Since `656ce38` the rootless suite has **no known failures** (the Snap symlink test was a real bug, fixed). Rerun before quoting numbers.
- The first-release gate is **open**: Debian 12/13, Fedora 43/44, aarch64, interactive Polkit agent, GTK launch per distro, firewalld/SELinux and CI runs are NOT VERIFIED.
- For the exact state, first run `rtk git status --short --branch`, then read the newest record in `docs/handoff.md`.
- At the end of a session, record the new state and the next narrow step in `docs/handoff.md`.

## Useful skills

- `superpowers:test-driven-development`: a real RED assertion first for journal/legacy/rollback behavior.
- `superpowers:systematic-debugging`: evidence → hypothesis → narrow test for strict-RPF and nft/kernel ABI issues.
- `superpowers:executing-plans`: implement the approved plan faithfully within task boundaries.
- `superpowers:verification-before-completion`: command output before any success claim.
