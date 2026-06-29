# RouteLane

RouteLane is a Linux desktop application for policy-based routing. It routes selected domains, IP addresses, or CIDR ranges through a chosen network interface while leaving the rest of the system traffic on the default route.

Example use case: route ChatGPT or OpenAI traffic through a VPN interface such as `tun0`, while regular browsing continues through your normal connection.

## Features

- GTK4 and Libadwaita desktop interface
- Domain, IP address, and CIDR routing rules
- Per-rule target interface selection
- Privileged network changes handled by `routelane-helper` through Polkit
- Kernel routing rules are removed when routing is disabled or the app exits
- Configuration is saved to `~/.config/routelane/config.json`
- Ubuntu `.deb` package build script

## How It Works

RouteLane keeps the GUI unprivileged. The desktop app stores rules, resolves domain targets, and sends routing operations to the privileged helper only when kernel routing changes are required.

```text
GTK / Libadwaita UI
        |
        | routing commands
        v
Tokio routing engine
        |
        | pkexec
        v
routelane-helper (root)
        |
        v
ip rule / ip route
```

Routing has two main states:

| State | Kernel routing state | Behavior |
| --- | --- | --- |
| Off | Empty | Rules are kept in the app configuration only. |
| On | Active | Rules are applied to the kernel through `routelane-helper`. |

## Requirements

Runtime requirements on Ubuntu or a compatible distribution:

- `libgtk-4-1`
- `libadwaita-1-0`
- `policykit-1`
- `iproute2`

Build requirements:

- Rust stable toolchain with `cargo`
- `libgtk-4-dev`
- `libadwaita-1-dev`
- `dpkg-deb` for `.deb` package builds

Install build dependencies on Ubuntu:

```bash
sudo apt install cargo libgtk-4-dev libadwaita-1-dev dpkg-dev
```

## Install from a .deb Package

The preferred local install path is the generated `.deb` package.

```bash
./packaging/deb/build-deb.sh
sudo apt install ./dist/routelane_0.1.0_amd64.deb
```

The package installs:

| Source | Installed path |
| --- | --- |
| `routelane` | `/usr/bin/routelane` |
| `routelane-helper` | `/usr/lib/routelane/routelane-helper` |
| `data/io.github.routelane.desktop` | `/usr/share/applications/io.github.routelane.desktop` |
| `data/io.github.routelane.policy` | `/usr/share/polkit-1/actions/io.github.routelane.policy` |

After installation, start RouteLane from the application launcher or run:

```bash
routelane
```

## Build from Source

```bash
git clone <repo-url>
cd routelane
cargo build --release --bins
```

Build outputs:

| Binary | Path |
| --- | --- |
| GUI application | `target/release/routelane` |
| Privileged helper | `target/release/routelane-helper` |

Manual installation from a source build:

```bash
sudo install -d -m 0755 /usr/lib/routelane
sudo install -m 0755 target/release/routelane-helper /usr/lib/routelane/routelane-helper
sudo install -m 0755 target/release/routelane /usr/bin/routelane
sudo install -m 0644 data/io.github.routelane.desktop /usr/share/applications/io.github.routelane.desktop
sudo install -m 0644 data/io.github.routelane.policy /usr/share/polkit-1/actions/io.github.routelane.policy
```

## Development Run

For local development, build both binaries and point the GUI to the development helper with `ROUTELANE_HELPER`:

```bash
cargo build --bins
ROUTELANE_HELPER=./target/debug/routelane-helper cargo run --bin routelane
```

The Polkit policy must still be installed before `routelane-helper` can run through `pkexec`.

## Usage

1. Start RouteLane.
2. Select the network interface that should carry routed traffic, for example `tun0`, `wg0`, or `wlan0`.
3. Add a domain, IP address, or CIDR target such as `chatgpt.com`, `8.8.8.8`, or `203.0.113.0/24`.
4. Turn routing on. Polkit may ask for administrator authentication.
5. Turn routing off or close the app to remove the active kernel routing rules.

Saved rules are restored when the app starts again. Routing starts disabled; the user must enable it manually.

## Configuration

RouteLane stores its configuration at:

```text
~/.config/routelane/config.json
```

Example:

```json
{
  "alt_interface": "tun0",
  "rules": [
    { "target_str": "chatgpt.com", "is_domain": true, "interface": "tun0" },
    { "target_str": "8.8.8.8", "is_domain": false, "interface": "tun0" }
  ]
}
```

## Security Model

- The `routelane` GUI does not run as root.
- Privileged operations are isolated in `routelane-helper`.
- The helper is launched through `pkexec` and authorized by `/usr/share/polkit-1/actions/io.github.routelane.policy`.
- The installed helper path is `/usr/lib/routelane/routelane-helper`.
- Helper input is validated before any `ip rule` or `ip route` command is executed.
- RouteLane uses routing table `100` and rule priorities in the `10000` to `10999` range.
- Kernel routing rules are removed when routing is disabled or the app exits.

## CDN and Domain Routing Limitations

Domain rules are resolved to IP addresses before kernel routing rules are applied. This approach works for stable DNS answers, but it has limits with CDN-backed services.

Domains such as `chatgpt.com` can return different IP addresses across DNS requests, locations, or time. When that happens, traffic may use an IP address that was not present when the route was applied. For long-running or high-accuracy domain routing, a DNS-integrated design such as `dnsmasq` plus `ipset` and packet marking is more reliable. The experimental backend for that direction lives in `src/routing/dns_router.rs`.

## Project Layout

```text
src/
  main.rs                  Application entry point
  config.rs                JSON configuration persistence
  models.rs                Shared data types and channel messages
  routing/
    mod.rs                 Routing engine loop
    manager.rs             Routing state manager
    executor.rs            pkexec / routelane-helper execution
    resolver.rs            Async DNS resolution
    dns_router.rs          Experimental dnsmasq/ipset backend
  ui/
    window.rs              Main window
    rule_row.rs            Rule row widget
    settings.rs            Settings UI
    tray.rs                Tray integration
    i18n.rs                UI text helpers
  bin/
    routelane_helper.rs    Privileged helper binary
data/
  io.github.routelane.desktop
  io.github.routelane.policy
packaging/
  deb/
    build-deb.sh           Local Debian package builder
```

## License

MIT
