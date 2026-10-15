# RouteLane

RouteLane is a Linux desktop application for policy-based multi-lane routing.
It can route selected domains, IP addresses, or CIDR ranges through different
network interfaces while leaving unmatched traffic on the normal system route.

Phase 1 includes:

- GTK4/Libadwaita multi-lane profile management
- `Route`, `Direct`, and independently enabled/disabled `Block` rules
- IPv4 and IPv6 policy routing with nftables marking
- per-application lane selection with cgroup v2 process affinity
- a per-user daemon and versioned Unix-socket IPC
- a small system D-Bus helper authorized by Polkit
- kernel-derived Connection Health reports
- transactional apply, rollback, ownership checks, and conflict diagnostics

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
1 surface is intentionally limited to the commands above.

`apply-profile` reads the same schema-version-2 configuration document as the
desktop application. When the document has one profile, no selector is needed.
For multiple profiles, `--profile <id-or-name>` is required; missing, duplicate,
ambiguous, or unknown selectors fail locally before daemon contact. Bare
profile JSON is not accepted. Input must be a current-user-owned regular file
without group/other write permission and is capped at 1 MiB; symlinks, FIFOs,
devices, and oversized files are rejected.

```json
{
  "schema_version": 2,
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
      "rules": []
    }
  ]
}
```

The desktop configuration is schema version 2 and is stored at
`~/.config/routelane/config.json`. Stored profiles are inactive on load; a user
must explicitly apply one.

## Uninstalling

```bash
sudo apt remove routelane
```

`apt remove` deletes every file the package owns (there are no conffiles), stops
and disables `routelane-helper.service`, and best-effort stops
`routelane-daemon.service` for any active user session. `~/.config/routelane/config.json`
is not owned by the package and is left in place for each user.

Disable routing (GUI toggle or `routelanectl deactivate`) **before** removing the
package if routing is currently active. Tearing down the live kernel routing
state (the `inet routelane` nftables table, rule priorities 10000-10099, and
routing tables 20000-20099) requires RouteLane's privileged Polkit action,
which needs an interactive prompt — a package removal script cannot satisfy
that non-interactively, so it cannot be done automatically during `apt remove`.
If you remove the package while routing was still active, `postrm` detects and
warns about any leftover kernel state and prints the exact commands to clear it
manually.

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

## Testing

Rootless checks:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --bins
./packaging/deb/build-deb.sh
```

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
skip, not a pass, and must be reported as **NOT VERIFIED**.

## License

RouteLane is licensed under the GNU General Public License version 3 only
(`GPL-3.0-only`). See `LICENSE`.
