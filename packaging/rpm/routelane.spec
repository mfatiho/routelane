# Built by packaging/rpm/build-rpm.sh from prebuilt release binaries; the
# owned files, modes and lifecycle mirror packaging/deb/build-deb.sh.
%global debug_package %{nil}
%global _build_id_links none

Name:           routelane
Version:        %{routelane_version}
Release:        1%{?dist}
Summary:        Policy-based routing desktop application
License:        GPL-3.0-only
URL:            https://github.com/routelane/routelane
Requires:       gtk4
Requires:       libadwaita
Requires:       nftables
Requires:       iproute
Requires:       polkit
Requires:       dbus
Requires:       systemd

%description
RouteLane routes selected domains or IP/CIDR targets through a chosen network
interface.

%prep

%build

%install
install -D -m 0755 %{routelane_root}/target/release/routelane %{buildroot}/usr/bin/routelane
install -D -m 0755 %{routelane_root}/target/release/routelane-daemon %{buildroot}/usr/bin/routelane-daemon
install -D -m 0755 %{routelane_root}/target/release/routelanectl %{buildroot}/usr/bin/routelanectl
install -D -m 0755 %{routelane_root}/target/release/routelane-helper %{buildroot}/usr/lib/routelane/routelane-helper
install -D -m 0644 %{routelane_root}/data/routelane-daemon.service %{buildroot}/usr/lib/systemd/user/routelane-daemon.service
install -D -m 0644 %{routelane_root}/data/routelane-helper.service %{buildroot}/usr/lib/systemd/system/routelane-helper.service
install -D -m 0644 %{routelane_root}/data/routelane-tmpfiles.conf %{buildroot}/usr/lib/tmpfiles.d/routelane.conf
install -D -m 0644 %{routelane_root}/data/io.github.routelane.Helper.service %{buildroot}/usr/share/dbus-1/system-services/io.github.routelane.Helper.service
install -D -m 0644 %{routelane_root}/data/io.github.routelane.Helper.conf %{buildroot}/usr/share/dbus-1/system.d/io.github.routelane.Helper.conf
install -D -m 0644 %{routelane_root}/data/io.github.routelane.desktop %{buildroot}/usr/share/applications/io.github.routelane.desktop
install -D -m 0644 %{routelane_root}/data/io.github.routelane.policy %{buildroot}/usr/share/polkit-1/actions/io.github.routelane.policy

# Install and upgrade ($1 >= 1), as the deb postinst "configure".
%post
if command -v systemd-tmpfiles >/dev/null 2>&1; then
    systemd-tmpfiles --create /usr/lib/tmpfiles.d/routelane.conf || :
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || :
    systemctl try-restart routelane-helper.service || :
    for run_dir in /run/user/*; do
        [ -d "$run_dir" ] || continue
        uid="${run_dir##*/}"
        user="$(getent passwd "$uid" | cut -d: -f1)" || continue
        [ -n "$user" ] || continue
        runuser -u "$user" -- env XDG_RUNTIME_DIR="$run_dir" \
            systemctl --user daemon-reload >/dev/null 2>&1 || :
        runuser -u "$user" -- env XDG_RUNTIME_DIR="$run_dir" \
            systemctl --user try-restart routelane-daemon.service >/dev/null 2>&1 || :
    done
fi

# Removal only ($1 == 0), as the deb prerm "remove"; upgrades keep running.
%preun
if [ "$1" -eq 0 ]; then
    systemctl stop routelane-helper.service >/dev/null 2>&1 || :
    systemctl disable routelane-helper.service >/dev/null 2>&1 || :
    for run_dir in /run/user/*; do
        [ -d "$run_dir" ] || continue
        uid="${run_dir##*/}"
        user="$(getent passwd "$uid" | cut -d: -f1)" || continue
        [ -n "$user" ] || continue
        runuser -u "$user" -- env XDG_RUNTIME_DIR="$run_dir" \
            systemctl --user stop routelane-daemon.service >/dev/null 2>&1 || :
    done
fi

%postun
if [ "$1" -eq 0 ]; then
    if [ -d /run/systemd/system ]; then
        systemctl daemon-reload >/dev/null 2>&1 || :
        systemctl reset-failed routelane-helper.service >/dev/null 2>&1 || :
    fi
    LEFTOVER=""
    if command -v ip >/dev/null 2>&1 \
        && ip rule show 2>/dev/null | grep -Eq '^100[0-9][0-9]:'; then
        LEFTOVER="1"
    fi
    if command -v nft >/dev/null 2>&1 \
        && nft list table inet routelane >/dev/null 2>&1; then
        LEFTOVER="1"
    fi
    echo "RouteLane has been removed. Each user's ~/.config/routelane/config.json was left in place." >&2
    if [ -n "$LEFTOVER" ]; then
        cat >&2 <<'EOF'
RouteLane was removed while its routing state was still active in the
kernel. This cannot be cleaned up automatically from a package removal
script -- RouteLane's privileged action requires an interactive Polkit
prompt that a non-interactive removal script cannot satisfy. Check for
and remove any leftover state manually:

    ip rule show | grep -E '^100[0-9][0-9]:'
    ip route show table 20000
    nft list table inet routelane

Remove what you find with 'sudo ip rule del ...', 'sudo ip route flush
table <n>', and 'sudo nft delete table inet routelane'.
EOF
    fi
fi

%files
%attr(0755, root, root) /usr/bin/routelane
%attr(0755, root, root) /usr/bin/routelane-daemon
%attr(0755, root, root) /usr/bin/routelanectl
%dir %attr(0755, root, root) /usr/lib/routelane
%attr(0755, root, root) /usr/lib/routelane/routelane-helper
%attr(0644, root, root) /usr/lib/systemd/user/routelane-daemon.service
%attr(0644, root, root) /usr/lib/systemd/system/routelane-helper.service
%attr(0644, root, root) /usr/lib/tmpfiles.d/routelane.conf
%attr(0644, root, root) /usr/share/dbus-1/system-services/io.github.routelane.Helper.service
%attr(0644, root, root) /usr/share/dbus-1/system.d/io.github.routelane.Helper.conf
%attr(0644, root, root) /usr/share/applications/io.github.routelane.desktop
%attr(0644, root, root) /usr/share/polkit-1/actions/io.github.routelane.policy
