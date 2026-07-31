# RouteLane

RouteLane is a Linux desktop application for policy-based multi-lane routing.
It can route selected domains, IP addresses, or CIDR ranges through different
network interfaces while leaving unmatched traffic on the normal system route.

Phase 1 includes:

- GTK4/Libadwaita multi-lane profile management
- `Route`, `Direct`, and independently enabled/disabled `Block` rules
- IPv4 and IPv6 policy routing with nftables marking
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
2. Choose a lane/interface in Preferences when the setup card asks for one.
3. Add a domain, IP, or CIDR rule, then select Enable routing.
4. Complete the desktop Polkit prompt when RouteLane needs to modify routing.

RouteLane never asks for or stores a sudo password. Polkit controls the
authorization lifetime, and a missing desktop Polkit agent yields
`NeedsAuthorization` rather than silent escalation.

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
Its runtime dependencies include `nftables`, `iproute2`, `policykit-1`, `dbus`,
and `systemd`, in addition to GTK/Libadwaita.

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

Root namespace integration is separate:

```bash
./scripts/run-root-integration.sh
```

Exit 77 means the environment cannot run the root namespace suite. It is a
skip, not a pass, and must be reported as **NOT VERIFIED**.

## License

RouteLane is licensed under the GNU General Public License version 3 only
(`GPL-3.0-only`). See `LICENSE`.
