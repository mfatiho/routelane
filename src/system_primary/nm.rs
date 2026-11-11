//! NetworkManager's own D-Bus authority; never runs a shell or RouteLane helper.
use super::*;
use crate::backend::{capture_system_topology, IpFamily};
use zbus::{
    proxy::{Builder, CacheProperties, MethodFlags},
    zvariant::OwnedObjectPath,
    Connection, Proxy,
};

const NM: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const PROFILE: &str = "org.freedesktop.NetworkManager.Settings.Connection";

pub struct NetworkManager {
    connection: Connection,
    owner: String,
}
fn bus_error(e: impl std::fmt::Display) -> Error {
    let detail = e.to_string();
    let kind = if detail.contains("Denied")
        || detail.contains("NotAuthorized")
        || detail.contains("Permission")
    {
        ErrorKind::Authorization
    } else {
        ErrorKind::Unavailable
    };
    Error::new(kind, detail)
}

fn text(settings: &Settings, section: &str, key: &str) -> Option<String> {
    settings
        .get(section)?
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .map(str::to_owned)
}

fn validate_settings(settings: &Settings) -> Result<()> {
    let connection_type = text(settings, "connection", "type").unwrap_or_default();
    if !["802-3-ethernet", "802-11-wireless"].contains(&connection_type.as_str())
        || text(settings, "connection", "controller").is_some()
        || text(settings, "connection", "master").is_some()
    {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "only independent Ethernet/Wi-Fi profiles are supported",
        ));
    }
    let ipv4 = settings
        .get("ipv4")
        .ok_or_else(|| Error::new(ErrorKind::Unsupported, "IPv4 settings missing"))?;
    if ipv4
        .get("never-default")
        .map(|v| bool::try_from(v).unwrap_or(true))
        .unwrap_or(false)
        || ![0, 254].contains(
            &ipv4
                .get("route-table")
                .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
                .unwrap_or(0),
        )
    {
        return Err(Error::new(
            ErrorKind::Conflict,
            "profile forbids a main-table default",
        ));
    }
    if metric(settings)?.is_some_and(|v| v < -1 || v > i64::from(u32::MAX)) {
        return Err(Error::new(ErrorKind::Unsupported, "invalid route metric"));
    }
    // Explicit per-route metrics cannot safely be replaced by this one setting.
    if let Some(routes) = ipv4.get("route-data") {
        let routes: Vec<std::collections::HashMap<String, OwnedValue>> = routes
            .try_clone()
            .map_err(bus_error)?
            .try_into()
            .map_err(bus_error)?;
        if routes
            .iter()
            .any(|r| r.get("prefix").and_then(|v| u32::try_from(v).ok()) == Some(0))
        {
            return Err(Error::new(
                ErrorKind::Conflict,
                "explicit static default route needs manual review",
            ));
        }
    }
    if let Some(routes) = ipv4.get("routes") {
        let routes: Vec<Vec<u32>> = routes
            .try_clone()
            .map_err(bus_error)?
            .try_into()
            .map_err(bus_error)?;
        if routes.iter().any(|r| r.get(1) == Some(&0)) {
            return Err(Error::new(
                ErrorKind::Conflict,
                "explicit legacy default route needs manual review",
            ));
        }
    }
    Ok(())
}

fn patch_metric(mut settings: Settings, value: Option<i64>) -> Result<Settings> {
    validate_settings(&settings)?;
    let ipv4 = settings.get_mut("ipv4").expect("validated IPv4");
    if let Some(value) = value {
        ipv4.insert("route-metric".into(), value.into());
    } else {
        ipv4.remove("route-metric");
    }
    Ok(settings)
}

fn default_metrics(table: &str) -> Result<Vec<(String, u32)>> {
    let mut result = vec![];
    for line in table.lines().skip(1) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 8 {
            return Err(Error::new(
                ErrorKind::Unavailable,
                "malformed main route table",
            ));
        }
        if fields[1] == "00000000" && fields[7] == "00000000" {
            let flags = u32::from_str_radix(fields[3], 16).map_err(bus_error)?;
            if flags & 1 != 0 {
                result.push((fields[0].into(), fields[6].parse().map_err(bus_error)?));
            }
        }
    }
    result.sort();
    result.dedup();
    Ok(result)
}

impl NetworkManager {
    pub async fn connect() -> Result<Self> {
        let connection = Connection::system().await.map_err(bus_error)?;
        Self::from_connection(connection).await
    }

    async fn from_connection(connection: Connection) -> Result<Self> {
        let dbus = Proxy::new(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        .map_err(bus_error)?;
        let owner: String = dbus.call("GetNameOwner", &(NM,)).await.map_err(bus_error)?;
        drop(dbus);
        let client = Self { connection, owner };
        let version: String = client
            .proxy(ROOT, NM)
            .await?
            .get_property("Version")
            .await
            .map_err(bus_error)?;
        let numbers = version
            .split('.')
            .take(2)
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(bus_error)?;
        if numbers.as_slice() < [1, 44].as_slice() {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "safe priority changes require NetworkManager 1.44 or newer",
            ));
        }
        Ok(client)
    }

    async fn proxy<'a>(&'a self, path: &'a str, interface: &'a str) -> Result<Proxy<'a>> {
        Builder::new(&self.connection)
            .destination(self.owner.as_str())
            .map_err(bus_error)?
            .path(path)
            .map_err(bus_error)?
            .interface(interface)
            .map_err(bus_error)?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .map_err(bus_error)
    }

    async fn paths(&self, interface: &str) -> Result<(OwnedObjectPath, OwnedObjectPath)> {
        let root = self.proxy(ROOT, NM).await?;
        let device: OwnedObjectPath = root
            .call("GetDeviceByIpIface", &(interface,))
            .await
            .map_err(bus_error)?;
        let proxy = self.proxy(device.as_str(), DEVICE).await?;
        let state: u32 = proxy.get_property("State").await.map_err(bus_error)?;
        if state != 100 {
            return Err(Error::new(
                ErrorKind::Disconnected,
                "selected network connection is not activated",
            ));
        }
        let active: OwnedObjectPath = proxy
            .get_property("ActiveConnection")
            .await
            .map_err(bus_error)?;
        let profile = self
            .proxy(active.as_str(), ACTIVE)
            .await?
            .get_property("Connection")
            .await
            .map_err(bus_error)?;
        Ok((device, profile))
    }
}

#[async_trait]
impl Backend for NetworkManager {
    async fn snapshot(&mut self, interface: &str) -> Result<Snapshot> {
        let (device, profile) = self.paths(interface).await?;
        let stored = self.proxy(profile.as_str(), PROFILE).await?;
        let version: u64 = stored.get_property("VersionId").await.map_err(bus_error)?;
        let saved: Settings = stored.call("GetSettings", &()).await.map_err(bus_error)?;
        let after: u64 = stored.get_property("VersionId").await.map_err(bus_error)?;
        if version == 0 || version != after {
            return Err(Error::new(
                ErrorKind::Stale,
                "profile changed during observation",
            ));
        }
        let (applied, applied_version): (Settings, u64) = self
            .proxy(device.as_str(), DEVICE)
            .await?
            .call("GetAppliedConnection", &(0u32,))
            .await
            .map_err(bus_error)?;
        validate_settings(&saved)?;
        validate_settings(&applied)?;
        let uuid = text(&saved, "connection", "uuid")
            .ok_or_else(|| Error::new(ErrorKind::Unavailable, "profile UUID missing"))?;
        if text(&applied, "connection", "uuid").as_deref() != Some(&uuid) || applied_version == 0 {
            return Err(Error::new(
                ErrorKind::Stale,
                "active and saved connection differ",
            ));
        }
        let topology = capture_system_topology().map_err(bus_error)?;
        let default_interface = topology
            .system_default(IpFamily::V4)
            .map_err(|e| Error::new(ErrorKind::Conflict, e.to_string()))?
            .map(str::to_owned);
        let entry = topology
            .interface(interface)
            .ok_or_else(|| Error::new(ErrorKind::Disconnected, "interface missing"))?;
        let identity = entry
            .identity
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::Disconnected, "interface identity missing"))?;
        let defaults =
            default_metrics(&std::fs::read_to_string("/proc/net/route").map_err(bus_error)?)?;
        let best = defaults.iter().min_by_key(|(_, m)| m).map(|(name, _)| name);
        if best.map(String::as_str) != default_interface.as_deref() {
            return Err(Error::new(
                ErrorKind::Stale,
                "kernel default changed during observation",
            ));
        }
        Ok(Snapshot {
            device_path: device.to_string(),
            profile_path: profile.to_string(),
            identity: Identity {
                interface: interface.into(),
                uuid,
                ifindex: identity.ifindex,
                hardware: identity
                    .hardware_address
                    .clone()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        Error::new(ErrorKind::Unsupported, "physical identity unavailable")
                    })?,
            },
            owner: self.owner.clone(),
            saved_version: version,
            applied_version,
            saved,
            applied,
            default_interface,
            defaults,
            ready: entry.ready(IpFamily::V4),
        })
    }

    async fn update(&mut self, expected: &Snapshot, metric: Option<i64>) -> Result<()> {
        let now = self.snapshot(&expected.identity.interface).await?;
        if approve(&now)? != approve(expected)? {
            return Err(Error::new(
                ErrorKind::Stale,
                "connection changed before save",
            ));
        }
        self.update_profile(
            &expected.profile_path,
            &expected.saved,
            expected.saved_version,
            metric,
        )
        .await
    }

    async fn reapply(&mut self, expected: &Snapshot, metric: Option<i64>) -> Result<()> {
        let now = self.snapshot(&expected.identity.interface).await?;
        if approve(&now)? != approve(expected)? {
            return Err(Error::new(
                ErrorKind::Stale,
                "connection changed before reapply",
            ));
        }
        self.reapply_device(
            &expected.device_path,
            &expected.applied,
            expected.applied_version,
            metric,
        )
        .await
    }
}

impl NetworkManager {
    async fn update_profile(
        &self,
        path: &str,
        original: &Settings,
        version: u64,
        metric: Option<i64>,
    ) -> Result<()> {
        let settings = patch_metric(original.clone(), metric)?;
        let args = BTreeMap::from([("version-id", OwnedValue::from(version))]);
        let result: Option<BTreeMap<String, OwnedValue>> = self
            .proxy(path, PROFILE)
            .await?
            .call_with_flags(
                "Update2",
                MethodFlags::AllowInteractiveAuth.into(),
                &(settings, 1u32, args),
            )
            .await
            .map_err(bus_error)?;
        result.ok_or_else(|| Error::new(ErrorKind::Incomplete, "save reply missing"))?;
        Ok(())
    }
    async fn reapply_device(
        &self,
        path: &str,
        original: &Settings,
        version: u64,
        metric: Option<i64>,
    ) -> Result<()> {
        let settings = patch_metric(original.clone(), metric)?;
        let result: Option<()> = self
            .proxy(path, DEVICE)
            .await?
            .call_with_flags(
                "Reapply",
                MethodFlags::AllowInteractiveAuth.into(),
                &(settings, version, 1u32),
            )
            .await
            .map_err(bus_error)?;
        result.ok_or_else(|| Error::new(ErrorKind::Incomplete, "reapply reply missing"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignores_host_routes_and_down_defaults_when_comparing_main_exits() {
        assert_eq!(default_metrics("Iface Destination Gateway Flags RefCnt Use Metric Mask\nusb0 00000000 0100A8C0 0003 0 0 113 00000000\neth0 00000000 0110970A 0003 0 0 114 00000000\nlab0 00C9410A 00000000 0001 0 0 0 00FFFFFF\nold0 00000000 00000000 0000 0 0 0 00000000\n").unwrap(), vec![("eth0".into(), 114), ("usb0".into(), 113)]);
    }

    fn ordinary_settings() -> Settings {
        BTreeMap::from([
            (
                "connection".into(),
                BTreeMap::from([(
                    "type".into(),
                    zbus::zvariant::Str::from("802-3-ethernet").into(),
                )]),
            ),
            (
                "ipv4".into(),
                BTreeMap::from([
                    ("method".into(), zbus::zvariant::Str::from("auto").into()),
                    ("dns-priority".into(), 100i32.into()),
                ]),
            ),
            (
                "ipv6".into(),
                BTreeMap::from([("method".into(), zbus::zvariant::Str::from("auto").into())]),
            ),
        ])
    }
    #[test]
    fn priority_payload_changes_only_the_approved_ipv4_field() {
        let original = ordinary_settings();
        let mut changed = patch_metric(original.clone(), Some(50)).unwrap();
        assert_eq!(metric(&changed).unwrap(), Some(50));
        changed.get_mut("ipv4").unwrap().remove("route-metric");
        assert_eq!(
            changed, original,
            "DNS, IPv6 and all unrelated settings must survive"
        );
    }
    #[test]
    fn never_default_or_policy_table_profiles_are_not_silently_rewritten() {
        for (key, value) in [
            ("never-default", OwnedValue::from(true)),
            ("route-table", OwnedValue::from(200u32)),
        ] {
            let mut original = ordinary_settings();
            original.get_mut("ipv4").unwrap().insert(key.into(), value);
            assert_eq!(
                patch_metric(original, Some(50)).unwrap_err().kind,
                ErrorKind::Conflict
            );
        }
    }

    #[tokio::test]
    #[ignore = "opt-in read-only NetworkManager observation; never calls Update2 or Reapply"]
    async fn local_networkmanager_readonly_observation() {
        let interface = std::env::var("ROUTELANE_NM_READONLY_INTERFACE")
            .expect("set ROUTELANE_NM_READONLY_INTERFACE to a connected Ethernet/Wi-Fi interface");
        let dir = tempfile::tempdir().unwrap();
        let mut manager = NetworkManager::connect().await.unwrap();
        let before = manager.snapshot(&interface).await.unwrap();
        let view = inspect(&mut manager, &dir.path().join("journal.json"), &interface)
            .await
            .unwrap();
        let after = manager.snapshot(&interface).await.unwrap();
        assert_eq!(view.approval.identity.interface, interface);
        assert_eq!(before.saved, after.saved);
        assert_eq!(before.applied, after.applied);
        assert!(!view.has_record);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[derive(Default)]
    struct Writes {
        saved: Option<Settings>,
        applied: Option<Settings>,
    }
    struct FakeRoot;
    #[zbus::interface(name = "org.freedesktop.NetworkManager")]
    impl FakeRoot {
        #[zbus(property)]
        fn version(&self) -> &str {
            "1.46.0"
        }
    }
    struct FakeProfile(std::sync::Arc<std::sync::Mutex<Writes>>);
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
    impl FakeProfile {
        fn update2(
            &self,
            settings: Settings,
            flags: u32,
            args: BTreeMap<String, OwnedValue>,
            #[zbus(header)] header: zbus::message::Header<'_>,
        ) -> zbus::fdo::Result<BTreeMap<String, OwnedValue>> {
            if flags != 1
                || args.get("version-id").and_then(|v| u64::try_from(v).ok()) != Some(42)
                || !header
                    .primary()
                    .flags()
                    .contains(zbus::message::Flags::AllowInteractiveAuth)
            {
                return Err(zbus::fdo::Error::InvalidArgs(
                    "expected durable CAS update with interactive authorization".into(),
                ));
            }
            self.0.lock().unwrap().saved = Some(settings);
            Ok(BTreeMap::new())
        }
    }
    struct FakeDevice(std::sync::Arc<std::sync::Mutex<Writes>>);
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
    impl FakeDevice {
        fn reapply(
            &self,
            settings: Settings,
            version: u64,
            flags: u32,
            #[zbus(header)] header: zbus::message::Header<'_>,
        ) -> zbus::fdo::Result<()> {
            if version != 9
                || flags != 1
                || !header
                    .primary()
                    .flags()
                    .contains(zbus::message::Flags::AllowInteractiveAuth)
            {
                return Err(zbus::fdo::Error::InvalidArgs(
                    "expected versioned reapply preserving external IP state".into(),
                ));
            }
            self.0.lock().unwrap().applied = Some(settings);
            Ok(())
        }
    }
    #[tokio::test]
    #[ignore = "private D-Bus only; run scripts/run-gtk-smoke.sh"]
    async fn nm_private_bus_write_contract() {
        assert_eq!(
            std::env::var("ROUTELANE_GTK_TEST_SESSION").as_deref(),
            Ok("1"),
            "requires the isolated test script"
        );
        assert_eq!(
            std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap(),
            std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap(),
            "must not contact the host system bus"
        );
        let writes = std::sync::Arc::new(std::sync::Mutex::new(Writes::default()));
        let profile = "/org/freedesktop/NetworkManager/Settings/1";
        let device = "/org/freedesktop/NetworkManager/Devices/2";
        let _server = zbus::connection::Builder::session()
            .unwrap()
            .name(NM)
            .unwrap()
            .serve_at(ROOT, FakeRoot)
            .unwrap()
            .serve_at(profile, FakeProfile(writes.clone()))
            .unwrap()
            .serve_at(device, FakeDevice(writes.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = NetworkManager::from_connection(Connection::session().await.unwrap())
            .await
            .unwrap();
        let settings = ordinary_settings();
        client
            .update_profile(profile, &settings, 42, Some(50))
            .await
            .unwrap();
        client
            .reapply_device(device, &settings, 9, Some(50))
            .await
            .unwrap();
        let before = writes.lock().unwrap().saved.clone().unwrap();
        assert_eq!(metric(&before).unwrap(), Some(50));
        assert_eq!(
            writes.lock().unwrap().applied.as_ref().unwrap()["ipv6"],
            settings["ipv6"]
        );
        assert!(client
            .update_profile(profile, &settings, 41, Some(20))
            .await
            .is_err());
        assert_eq!(
            writes.lock().unwrap().saved.as_ref().unwrap(),
            &before,
            "bad versions cannot write"
        );
    }
}
