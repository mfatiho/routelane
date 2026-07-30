use std::fmt::Write;

use ipnet::IpNet;

use super::{CompiledAction, CompiledTarget, KernelPlan};

const TABLE_FAMILY: &str = "inet";
const TABLE_NAME: &str = "routelane";
const CHAIN_NAME: &str = "output";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableMode {
    Create,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftRenderError {
    InvalidInterfaceName,
}

impl std::fmt::Display for NftRenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInterfaceName => formatter.write_str("invalid kernel interface name"),
        }
    }
}

impl std::error::Error for NftRenderError {}

pub fn render_ruleset(plan: &KernelPlan, mode: TableMode) -> Result<String, NftRenderError> {
    if plan
        .lanes
        .iter()
        .any(|lane| !is_kernel_interface_name(&lane.interface))
    {
        return Err(NftRenderError::InvalidInterfaceName);
    }

    let mut batch = String::new();
    if mode == TableMode::Replace {
        writeln!(batch, "delete table {TABLE_FAMILY} {TABLE_NAME}").unwrap();
    }
    writeln!(batch, "add table {TABLE_FAMILY} {TABLE_NAME}").unwrap();
    writeln!(
        batch,
        "add chain {TABLE_FAMILY} {TABLE_NAME} {CHAIN_NAME} {{ type route hook output priority mangle; policy accept; }}"
    )
    .unwrap();

    for rule in &plan.rules {
        write!(batch, "add rule {TABLE_FAMILY} {TABLE_NAME} {CHAIN_NAME}").unwrap();
        render_target(&mut batch, &rule.target);
        match rule.action {
            CompiledAction::Mark(mark) => {
                writeln!(batch, " meta mark set 0x{mark:08x} counter").unwrap();
            }
            CompiledAction::Direct => {
                writeln!(batch, " meta mark set 0x00000000 counter return").unwrap();
            }
            CompiledAction::Block => {
                writeln!(batch, " counter reject").unwrap();
            }
        }
    }

    Ok(batch)
}

fn render_target(batch: &mut String, target: &CompiledTarget) {
    let CompiledTarget::Network(network) = target else {
        return;
    };

    match network {
        IpNet::V4(network) => write!(batch, " ip daddr {}", render_v4_network(*network)).unwrap(),
        IpNet::V6(network) => write!(batch, " ip6 daddr {}", render_v6_network(*network)).unwrap(),
    }
}

fn render_v4_network(network: ipnet::Ipv4Net) -> String {
    if network.prefix_len() == 32 {
        network.addr().to_string()
    } else {
        network.to_string()
    }
}

fn render_v6_network(network: ipnet::Ipv6Net) -> String {
    if network.prefix_len() == 128 {
        network.addr().to_string()
    } else {
        network.to_string()
    }
}

fn is_kernel_interface_name(interface: &str) -> bool {
    !interface.is_empty()
        && interface.len() <= 15
        && interface
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}
