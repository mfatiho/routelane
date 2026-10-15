use super::{InterfaceIdentity, PrivilegedError};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceSettingReceipt {
    pub identity: InterfaceIdentity,
    pub previous: u8,
    pub applied: u8,
}

pub trait MarkAwareRpfBackend: Send {
    fn identity(&mut self, name: &str) -> Result<InterfaceIdentity, PrivilegedError>;
    fn rp_filter(&mut self, name: &str) -> Result<u8, PrivilegedError>;
    fn src_valid_mark(&mut self, name: &str) -> Result<u8, PrivilegedError>;
    fn set_src_valid_mark(
        &mut self,
        identity: &InterfaceIdentity,
        value: u8,
    ) -> Result<(), PrivilegedError>;
}

fn valid_interface_scope(identity: &InterfaceIdentity) -> bool {
    identity.ifindex != 0 && valid_interface_name(&identity.name)
}

fn valid_interface_name(name: &str) -> bool {
    !matches!(name, "all" | "default" | "." | "..")
        && !name.is_empty()
        && name.len() <= 15
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Fixed-purpose proc adapter; there is no arbitrary path/value operation.
#[derive(Debug, Default)]
pub struct SystemMarkAwareRpfBackend;

impl SystemMarkAwareRpfBackend {
    fn read(name: &str, knob: &str) -> Result<u8, PrivilegedError> {
        // The caller supplies only the two literals below; global values are
        // read-only inputs, never writable targets of this adapter.
        if !matches!(knob, "rp_filter" | "src_valid_mark")
            || !(matches!(name, "all" | "default") || valid_interface_name(name))
        {
            return Err(PrivilegedError::new(
                "invalid fixed-purpose interface setting read",
            ));
        }
        let path = Path::new("/proc/sys/net/ipv4/conf").join(name).join(knob);
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| PrivilegedError::new(format!("read interface {knob}: {e}")))?;
        let mut text = String::new();
        file.take(32)
            .read_to_string(&mut text)
            .map_err(|e| PrivilegedError::new(format!("read interface {knob}: {e}")))?;
        text.trim()
            .parse()
            .map_err(|_| PrivilegedError::new(format!("invalid interface {knob} value")))
    }

    fn revalidate(&mut self, identity: &InterfaceIdentity) -> Result<(), PrivilegedError> {
        if !valid_interface_scope(identity) || self.identity(&identity.name)? != *identity {
            return Err(PrivilegedError::new(
                "interface setting write identity is invalid or changed",
            ));
        }
        Ok(())
    }
}

impl MarkAwareRpfBackend for SystemMarkAwareRpfBackend {
    fn identity(&mut self, name: &str) -> Result<InterfaceIdentity, PrivilegedError> {
        if !valid_interface_name(name) {
            return Err(PrivilegedError::new(
                "interface setting requires a non-global interface",
            ));
        }
        let links = super::privileged::run_topology_query(super::topology::TopologyQuery::Links)
            .map_err(|e| PrivilegedError::new(e.to_string()))?;
        let topology = super::parse_topology(super::TopologyInput {
            links: &links,
            addresses: b"[]",
            routes_v4: b"[]",
            routes_v6: b"[]",
            rules_v4: b"[]",
            rules_v6: b"[]",
        })
        .map_err(|e| PrivilegedError::new(e.to_string()))?;
        topology
            .interface(name)
            .and_then(|entry| entry.identity.clone())
            .ok_or_else(|| {
                PrivilegedError::new("interface for mark-aware filtering no longer exists")
            })
    }
    fn rp_filter(&mut self, name: &str) -> Result<u8, PrivilegedError> {
        Self::read(name, "rp_filter")
    }
    fn src_valid_mark(&mut self, name: &str) -> Result<u8, PrivilegedError> {
        Self::read(name, "src_valid_mark")
    }
    fn set_src_valid_mark(
        &mut self,
        identity: &InterfaceIdentity,
        value: u8,
    ) -> Result<(), PrivilegedError> {
        if !valid_interface_scope(identity) || value > 1 {
            return Err(PrivilegedError::new(
                "mark-aware setting forbids global/default/invalid targets and values",
            ));
        }
        if unsafe { libc::geteuid() } != 0 {
            return Err(PrivilegedError::new(
                "mark-aware interface setting requires the privileged helper",
            ));
        }
        self.revalidate(identity)?;
        let path = Path::new("/proc/sys/net/ipv4/conf")
            .join(&identity.name)
            .join("src_valid_mark");
        let mut file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| PrivilegedError::new(format!("open interface src_valid_mark: {e}")))?;
        self.revalidate(identity)?;
        file.write_all(if value == 1 { b"1\n" } else { b"0\n" })
            .map_err(|e| PrivilegedError::new(format!("write interface src_valid_mark: {e}")))
    }
}

pub struct MarkAwareRpf<B, J> {
    backend: B,
    journal: J,
}

impl<B: MarkAwareRpfBackend, J: FnMut(&InterfaceSettingReceipt) -> Result<(), PrivilegedError>>
    MarkAwareRpf<B, J>
{
    pub fn new(backend: B, journal: J) -> Self {
        Self { backend, journal }
    }

    fn validate_identity(&mut self, identity: &InterfaceIdentity) -> Result<(), PrivilegedError> {
        if !valid_interface_scope(identity) {
            return Err(PrivilegedError::new(
                "interface setting requires an exact non-global kernel interface identity",
            ));
        }
        if self.backend.identity(&identity.name)? != *identity {
            return Err(PrivilegedError::new(
                "interface identity changed; refusing a setting mutation on another device",
            ));
        }
        Ok(())
    }

    pub fn ensure_mark_aware_rpf(
        &mut self,
        identity: &InterfaceIdentity,
    ) -> Result<Option<InterfaceSettingReceipt>, PrivilegedError> {
        self.validate_identity(identity)?;
        let global = self.backend.rp_filter("all")?;
        let local = self.backend.rp_filter(&identity.name)?;
        if global > 2 || local > 2 {
            return Err(PrivilegedError::new(
                "invalid observed reverse-path filtering value",
            ));
        }
        if global.max(local) != 1 {
            return Ok(None);
        }
        let global = self.backend.src_valid_mark("all")?;
        let previous = self.backend.src_valid_mark(&identity.name)?;
        if global > 1 || previous > 1 {
            return Err(PrivilegedError::new(
                "invalid observed mark-aware filtering value",
            ));
        }
        if global.max(previous) == 1 {
            return Ok(None);
        }
        let receipt = InterfaceSettingReceipt {
            identity: identity.clone(),
            previous,
            applied: 1,
        };
        // The callback must complete the durable journal save (including fsync)
        // before we can revalidate and perform the first setting write.
        (self.journal)(&receipt)?;
        self.validate_identity(identity)?;
        if self.backend.src_valid_mark(&identity.name)? != previous {
            return Err(PrivilegedError::new(
                "interface setting changed after journaling; refusing to overwrite it",
            ));
        }
        self.backend.set_src_valid_mark(identity, receipt.applied)?;
        if self.backend.src_valid_mark(&identity.name)? != receipt.applied {
            return Err(PrivilegedError::new(
                "mark-aware interface setting failed read-back verification",
            ));
        }
        Ok(Some(receipt))
    }

    pub fn restore_mark_aware_rpf(
        &mut self,
        receipt: &InterfaceSettingReceipt,
    ) -> Result<(), PrivilegedError> {
        if receipt.previous != 0 || receipt.applied != 1 {
            return Err(PrivilegedError::new(
                "invalid mark-aware interface setting receipt",
            ));
        }
        self.validate_identity(&receipt.identity)?;
        let current = self.backend.src_valid_mark(&receipt.identity.name)?;
        if current == receipt.previous {
            return Ok(());
        }
        if current != receipt.applied {
            return Err(PrivilegedError::new(
                "administrator changed interface setting; preserving current value",
            ));
        }
        self.validate_identity(&receipt.identity)?;
        self.backend
            .set_src_valid_mark(&receipt.identity, receipt.previous)?;
        if self.backend.src_valid_mark(&receipt.identity.name)? != receipt.previous {
            return Err(PrivilegedError::new(
                "restored interface setting failed read-back verification",
            ));
        }
        Ok(())
    }
}

impl<B: MarkAwareRpfBackend + ?Sized> MarkAwareRpfBackend for &mut B {
    fn identity(&mut self, name: &str) -> Result<InterfaceIdentity, PrivilegedError> {
        (**self).identity(name)
    }
    fn rp_filter(&mut self, name: &str) -> Result<u8, PrivilegedError> {
        (**self).rp_filter(name)
    }
    fn src_valid_mark(&mut self, name: &str) -> Result<u8, PrivilegedError> {
        (**self).src_valid_mark(name)
    }
    fn set_src_valid_mark(
        &mut self,
        identity: &InterfaceIdentity,
        value: u8,
    ) -> Result<(), PrivilegedError> {
        (**self).set_src_valid_mark(identity, value)
    }
}
