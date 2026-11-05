# RouteLane

RouteLane is a Linux desktop application for policy-based multi-lane routing.
It can route selected domains, IP addresses, CIDR ranges, or applications
through different network interfaces. While a profile is active, traffic that
matches no rule currently leaves through the primary connection; leaving it to
the operating system instead is being implemented (see
[Unmatched traffic](#unmatched-traffic-selective-routing)).

Current capabilities:

- GTK4/Libadwaita multi-lane profile management
- `Route`, `Direct`, and independently enabled/disabled `Block` rules
- IPv4 and IPv6 policy routing with nftables marking
- per-application lane selection with cgroup v2 process affinity
- a primary connection with approved backups and automatic, consent-bounded
  failover that keeps working while the GUI is closed
- a per-user daemon and versioned Unix-socket IPC
- a small system D-Bus helper authorized by Polkit
- kernel-derived Connection Health reports
- transactional apply, rollback, ownership checks, and conflict diagnostics
- development `.deb` and `.rpm` packages

## Architecture and security boundary

The GUI and `routelanectl` are unprivileged clients. They talk only to the
per-user daemon. The daemon compiles and validates profiles, then asks the
system helper to inspect or mutate the reserved RouteLane kernel resources.

```text
GTK UI / routelanectl
        |
        | versioned user Unix socket (0600)
        v
routelane-daemon
        |
        | system D-Bus + Polkit action io.github.routelane.apply
        v
routelane-helper (root, D-Bus activated)
        |
        +-- nftables table inet routelane
        +-- routing tables 20000..20099
        +-- rule priorities 10000..10099
        +-- marks 0x524c0001..0x524c0064
        +-- cgroups /sys/fs/cgroup/routelane.apps/*
```

The helper accepts typed plans rather than arbitrary commands. Before every
mutation it validates limits, exact resource ownership, generation, plan hash,
and live kernel state. A failed replacement is rolled back to the previous
generation. State journals are private root-owned files under `/run/routelane`.
The helper is activated through system D-Bus; it has no enable/install target
and is not intended to be started directly.

Inspection never replays a journal or changes kernel resources, even after an
interrupted transaction. Recovery is deferred to an authorized mutating call.
The runtime journal survives a helper service restart, but not a reboot.
Application cgroup writes are confined to `/sys/fs/cgroup/routelane.apps`;
the rest of the cgroup filesystem remains protected in the helper service.

RouteLane does not use an external “what is my IP” service for health checks.
Connection Health compares the desired plan with lossless local interface,
route, rule, nftables, ownership, and conflict evidence.

## Guided desktop journey

1. Open RouteLane; if it is already running, its existing window is focused.
2. In Connections, add a named network interface and choose what happens if it
   becomes unavailable. Blocking assigned traffic is the protective default.
3. Add an installed program in Applications, or a domain/IP/CIDR in Rules.
   Editors save only when you select Save; Cancel and Escape discard the draft.
   Direct and Block rules do not require a connection definition.
4. Select Enable routing in the persistent status area. Complete the desktop
   Polkit prompt if requested. Saved settings and verified applied settings are
   shown separately; a disconnected daemon is not presented as inactive.
5. Start an application after its saved profile is verified. Applying to running
   processes requires a scope confirmation; other windows and browser tabs may
   be affected, and existing connections are not guaranteed to move.

Overview summarizes every configured connection and local health evidence.
Rules and Applications keep Add and Search above their lists; contextual menus
provide editing, deletion and ordering without permanently expanded controls.
Ctrl+F focuses search, Ctrl+N opens the current section's editor, and Escape
cancels dialogs. Preferences contains language settings, with confirmation before
saving a language change and exiting for restart.

Application routing applies to every new connection from the selected process
tree, including CDN, authentication, and browser verification endpoints. For a
browser this also means other tabs in the same browser process use that lane.
RouteLane does not decrypt HTTPS or infer third-party domains.

### CIDR input and duplicate rules

The rule target accepts networks such as `10.65.201.0/24` without quotes.
Host bits are normalized: `10.65.201.25/24` becomes `10.65.201.0/24`.
Within a profile, an equivalent source, target and action/connection cannot
be saved twice, even with a different ID, priority or enabled state. Domain
case and a trailing dot are ignored; an IP and its `/32` or `/128` equivalent
also count as duplicates. Overlapping but distinct networks and different
actions remain valid. Editing a rule does not conflict with itself.

The editor shows an actionable warning; configuration loading/saving and CLI
input enforce the same rule. Older configuration files containing duplicates
are rejected, not silently rewritten: remove or adjust the reported duplicate
before importing them. Existing journaled kernel state remains eligible for
exact ownership-verified cleanup.

Distinct domains can legitimately resolve to the same IP. The planner emits
one identical family/target/verdict while retaining first-match order and the
saved domain rules. New helper ApplyPlan requests with duplicate compiled
verdicts are rejected before kernel mutation. RouteLane uses nftables, not an
additional iptables backend, and does not remove other programs' rules.

Domain rules currently use the system resolver's IP snapshot when a profile is
applied. Continuous DNS/TTL refresh is not implemented; a later IP change or a
browser using a different DNS resolver can therefore miss that snapshot.
Reapplying the profile refreshes it, but does not guarantee that a browser's
independent DNS answers will match. Application affinity avoids depending on
domain IP snapshots for new connections from supported host processes.
Connection Health verifies local routing resources, not successful loading of
an arbitrary website.

A domain rule matches only that exact name. Subdomains, HTTP redirects to
another host, and CDN or download hosts need their own rules: for example,
`https://chatgpt.com/codex/install.sh` redirects to `releases.openai.com`,
which a `chatgpt.com` rule does not cover. `curl -sIL <url>` shows a redirect
chain. Wildcard domains (`*.example.com`) and service packs exist in the
profile format but are rejected when a profile is applied; they are not yet
supported. CDN addresses are often shared, so a domain rule can also steer
other sites that use the same IP. For work that reaches many hosts, such as an
installer, an application rule for a dedicated terminal is more reliable.

Host `.desktop` applications with a resolvable executable are supported.
Flatpak and Snap applications are detected as sandboxed integration cases and
are not part of the first application-routing release; they require separate
host/sandbox mediation rather than silently bypassing the selected lane.

RouteLane never asks for or stores a sudo password. Polkit controls the
authorization lifetime, and a missing desktop Polkit agent yields
`NeedsAuthorization` rather than silent escalation.

### Primary connection and automatic failover

In Connections, choose which connection carries traffic that no rule assigns,
or keep following the system default, and order the approved backups.
Changing the primary asks for confirmation; Cancel and Escape change nothing.
Enabling the profile requests one Polkit authorization that covers exactly
that policy, those connections and that user for the current boot. If the
primary loses carrier, its address or its default route, the helper sends new
connections through the first available backup without another prompt, and
returns to the primary once it has been stable for five seconds. Any change
to the policy, the connections or the user needs a fresh authorization. An
address family with no available approved connection is blocked instead of
leaving through another interface. Existing connections are not moved.
Overview shows the saved preference separately from the verified effective
connection.

Failover reacts to local link evidence (carrier, address and default-route
loss), not to internet reachability: a link that stays up while the upstream
network fails does not trigger it. If a transition cannot be committed and
verified, the helper keeps the previous plan in force, reports `Degraded`, and
retries the same transition after an increasing wait (2 s up to 60 s); new
link evidence is acted on at once.

### Unmatched traffic (selective routing)

Every schema-version-4 profile records what happens to traffic that matches no
enabled rule in `routing.unmatched_traffic`:

| Value | Intended behavior |
| --- | --- |
| `PreserveSystem` | Unmatched traffic keeps the operating system's own routing; RouteLane does not mark, NAT or reroute it. Default for new and migrated profiles. |
| `UsePrimary` | Unmatched internet traffic is also sent through the primary connection and its approved backups. Requires the user's explicit choice. |

**Status:** the setting is stored, validated and migrated, but the kernel plan
does not use it yet. Until the selective routing work is complete, an active
profile sends unmatched traffic through the primary connection whatever the
value, so local bridges such as Multipass, Docker or other VPN networks can be
affected. The GUI does not offer the choice yet. The design and plan are
[docs/superpowers/specs/2026-10-22-selective-routing-design.md](docs/superpowers/specs/2026-10-22-selective-routing-design.md)
and
[docs/superpowers/plans/2026-10-22-selective-routing.md](docs/superpowers/plans/2026-10-22-selective-routing.md).

## Build

Build requirements:

- Rust stable
- GTK4 and Libadwaita development packages
- `dpkg-deb` only when producing the development `.deb`

```bash
cargo build --release --bins
```

The four binaries are:

| Binary | Purpose |
| --- | --- |
| `routelane` | GTK desktop application |
| `routelane-daemon` | per-user routing daemon |
| `routelanectl` | minimal Phase 1 command-line client |
| `routelane-helper` | privileged system D-Bus helper |

## Development `.deb`

```bash
./packaging/deb/build-deb.sh
```

The script builds in a private temporary root, creates a uniquely named
artifact under `dist/`, and verifies required paths, modes, and root ownership.
Its runtime dependencies include `nftables`, `iproute2`, `polkitd` (or
`policykit-1` on Ubuntu 22.04), `dbus`, and `systemd`, in addition to
GTK/Libadwaita.

This `.deb` is a Phase 1 development-verification artifact. Producing it does
not by itself establish official Ubuntu, Debian, or Fedora release support, and
it is not an RPM validation result.

Installed integration files:

| Path | Purpose |
| --- | --- |
| `/usr/bin/routelane` | GTK client |
| `/usr/bin/routelane-daemon` | user daemon |
| `/usr/bin/routelanectl` | CLI client |
| `/usr/lib/routelane/routelane-helper` | fixed, root-managed helper path |
| `/usr/lib/systemd/user/routelane-daemon.service` | user service |
| `/usr/lib/systemd/system/routelane-helper.service` | D-Bus-activated system service |
| `/usr/lib/tmpfiles.d/routelane.conf` | create the helper's isolated application cgroup subtree |
| `/usr/share/dbus-1/system-services/io.github.routelane.Helper.service` | D-Bus activation |
| `/usr/share/polkit-1/actions/io.github.routelane.policy` | Polkit policy |

## Development `.rpm`

```bash
./packaging/rpm/build-rpm.sh
```

Run it on Fedora with `rpm-build`, `cargo`, and the GTK4/Libadwaita
development packages. The rpm installs the same files, modes, and lifecycle
scripts as the `.deb`, with `polkit`, `nftables`, `iproute`, `dbus`, and
`systemd` as runtime dependencies, and is written under a unique name in
`dist/`. Building it is not a Fedora validation result; current evidence and
NOT VERIFIED rows are in `docs/primary-failover-verification.md`.

## Phase 1 usage

The GUI starts the user daemon through its normal desktop integration. For
manual testing:

```bash
systemctl --user start routelane-daemon.service
routelanectl status
routelanectl status --json
routelanectl apply-profile ./profile.json
routelanectl apply-profile ./config.json --profile Work
routelanectl deactivate
```

`routelanectl` uses the same canonical daemon IPC client as the GUI. Its Phase
1 surface is intentionally limited to the commands above. `status` also prints
the runtime phase and the preferred and effective exit for each address
family.

`apply-profile` reads the same schema-version-4 configuration document as the
desktop application. Schema-version-3, version-2 and legacy documents are
migrated to version 4 on load without being rewritten; rules, IDs, lanes and
the primary selection are kept, `unmatched_traffic` is proposed as
`PreserveSystem`, and every migrated profile is inactive. When the document has one profile, no selector is needed.
For multiple profiles, `--profile <id-or-name>` is required; missing, duplicate,
ambiguous, or unknown selectors fail locally before daemon contact. Bare
profile JSON is not accepted. Input must be a current-user-owned regular file
without group/other write permission and is capped at 1 MiB; symlinks, FIFOs,
devices, and oversized files are rejected.

```json
{
  "schema_version": 4,
  "language": "en",
  "profiles": [
    {
      "id": "work",
      "name": "Work",
      "enabled": false,
      "lanes": [
        {
          "id": "vpn",
          "name": "VPN",
          "interface": "wg0",
          "fallback_lane": null,
          "failure_policy": "Inactive"
        }
      ],
      "rules": [],
      "routing": {
        "unmatched_traffic": "PreserveSystem",
        "primary": "FollowSystemDefault",
        "backups": [],
        "automatic_failover": true
      }
    }
  ]
}
```

Every version-4 profile requires a `routing` object with all four fields;
unknown fields are rejected. `unmatched_traffic` is `"PreserveSystem"` or
`"UsePrimary"`, `primary` is either `"FollowSystemDefault"` or
`{"Lane": "<lane-id>"}`, and `backups` lists the lane IDs the user approved
for failover.

The desktop configuration is schema version 4 and is stored at
`~/.config/routelane/config.json`. Stored profiles are inactive on load; a user
must explicitly apply one.

## Uninstalling

```bash
sudo apt remove routelane    # Debian/Ubuntu
sudo dnf remove routelane    # Fedora
```

Removing the package deletes every file it owns (there are no conffiles), stops
and disables `routelane-helper.service`, and best-effort stops
`routelane-daemon.service` for any active user session. `~/.config/routelane/config.json`
is not owned by the package and is left in place for each user.

Disable routing (GUI toggle or `routelanectl deactivate`) **before** removing the
package if routing is currently active. Tearing down the live kernel routing
state (the `inet routelane` nftables table, rule priorities 10000-10099, and
routing tables 20000-20099) requires RouteLane's privileged Polkit action,
which needs an interactive prompt — a package removal script cannot satisfy
that non-interactively, so it cannot be done automatically during removal.
If you remove the package while routing was still active, the removal script
detects and warns about any leftover kernel state and prints the exact
commands to clear it manually. Upgrades only try-restart running services; the
ownership journal under `/run/routelane` and each user's configuration are
kept.

## Troubleshooting a GUI crash

The desktop launcher runs `RUST_BACKTRACE=1 routelane`, and its stderr,
including any panic message and backtrace, is captured by the user journal
under the app-launch scope. If the window closes unexpectedly right after
opening, check:

```bash
journalctl --user -g routelane --since "-10 min"
```

`cargo fmt`/`cargo test`/`cargo clippy` cannot catch a crash that only
happens once the compiled binary actually runs (for example, a `RefCell`
double-borrow reachable only from a live GTK callback) — a GUI crash always
needs an actual launch to reproduce and the journal to diagnose.

## Official first-release support gate

The first official RouteLane release will not be published until every cell in
this matrix is green in CI and release validation:

| Distribution | Versions | Architectures |
| --- | --- | --- |
| Ubuntu | 22.04, 24.04, 26.04 | x86_64, aarch64 |
| Debian | 12, 13 | x86_64, aarch64 |
| Fedora | 43, 44 | x86_64, aarch64 |

All three distribution families are in official support scope from the first
release. A package-format check on only one family is not enough to open the
release gate. Kernel namespace integration, package installation, systemd user
activation, system D-Bus activation, Polkit authorization, GTK launch, and
rollback/health scenarios must pass for every listed distribution and
architecture.

**Gate status: open.** Ubuntu 22.04, 24.04 and 26.04 on x86_64 have recorded
kernel, system-bus/Polkit, upgrade, reboot and removal runs for the primary
failover work (26.04 with intermittent failures in two counter- and
threshold-based root tests). Debian, Fedora (the rpm was only built in a
container), aarch64, the interactive Polkit agent, GTK launch per distribution
and CI are **NOT VERIFIED**. Selective routing has no kernel evidence yet.
Details: [docs/primary-failover-verification.md](docs/primary-failover-verification.md).

## Testing

See [the reliability audit](docs/reliability-audit.md) for earlier fixes and
[the failover verification record](docs/primary-failover-verification.md) for
the current kernel, package and distribution evidence. Historical results are
not current evidence; rerun the commands below.

Rootless checks:

```bash
cargo fmt --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --bins
./packaging/deb/build-deb.sh
```

`tests/primary_packaging_contract.rs` checks that the deb and rpm ship the
same files and lifecycle and that the helper keeps its hardening.

The real GTK window and add dialogs can also be tested without the user's
desktop session or configuration. This needs `dbus-run-session` and
`gtk4-broadwayd`, but no root access or new Rust dependencies:

```bash
bash scripts/run-gtk-smoke.sh
```

The script uses a private session bus and temporary configuration/runtime
directory. It opens the actual window, exercises both add dialogs, and checks
that refresh preserves search, row identity and focus. It also covers safe
dialog cancellation, validation, language confirmation and once-only application
launch submission. It is a smoke test, not end-to-end network verification.

Root namespace integration is separate:

```bash
./scripts/run-root-integration.sh
```

Exit 77 means the environment cannot run the root namespace suite. It is a
skip, not a pass, and must be reported as **NOT VERIFIED**. Run it only in an
authorized disposable VM, never on a workstation. The real system-bus/Polkit
acceptance (`tests/root/system_bus_acceptance.sh`) installs the package and
refuses to run unless `ROUTELANE_DISPOSABLE_VM` equals the VM's hostname.

## License

RouteLane is licensed under the GNU General Public License version 3 only
(`GPL-3.0-only`). See `LICENSE`.
