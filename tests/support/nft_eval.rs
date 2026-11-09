//! Minimal nftables JSON evaluator for contract tests: walks RouteLane's
//! rendered chains for one synthetic packet. Test-only; never used by the product.
use std::net::IpAddr;

use ipnet::IpNet;
use serde_json::{json, Value};

pub fn rules(document: &Value, chain: &str) -> Vec<Value> {
    document["nftables"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|o| o.get("rule"))
        .filter(|r| r["chain"] == chain)
        .map(|r| r["expr"].clone())
        .collect()
}

#[derive(Clone, Debug)]
pub struct Packet {
    pub family: &'static str,
    pub source: &'static str,
    pub destination: &'static str,
    pub mark: u32,
    pub ct_mark: u32,
    pub state: &'static str,
    pub direction: &'static str,
    pub iif: u32,
    pub iifname: &'static str,
    pub oif: u32,
    pub oifname: &'static str,
    pub l4proto: &'static str,
    pub sport: u16,
    pub dport: u16,
    pub hoplimit: u8,
    pub icmp_type: &'static str,
    pub translated: bool,
    /// cgroup v2 path (level 2), e.g. `routelane.apps/browser`; empty = not in RouteLane.
    pub cgroup: &'static str,
}

impl Default for Packet {
    fn default() -> Self {
        Self {
            family: "ipv4",
            source: "192.168.0.2",
            destination: "203.0.113.10",
            mark: 0,
            ct_mark: 0,
            state: "new",
            direction: "original",
            iif: 0,
            iifname: "",
            oif: 2,
            oifname: "eth0",
            translated: false,
            cgroup: "",
            l4proto: "tcp",
            sport: 0,
            dport: 0,
            hoplimit: 64,
            icmp_type: "echo-request",
        }
    }
}

fn value(expression: &Value, packet: &Packet) -> Value {
    if let Some(meta) = expression.get("meta") {
        return match meta["key"].as_str().unwrap() {
            "nfproto" => json!(packet.family),
            "mark" => json!(packet.mark),
            "iif" => json!(packet.iif),
            "iifname" => json!(packet.iifname),
            "oif" => json!(packet.oif),
            "oifname" => json!(packet.oifname),
            "l4proto" => json!(packet.l4proto),
            key => panic!("unsupported meta {key}"),
        };
    }
    if let Some(ct) = expression.get("ct") {
        return match ct["key"].as_str().unwrap() {
            "mark" => json!(packet.ct_mark),
            "direction" => json!(packet.direction),
            "state" => json!(packet.state),
            // Like the kernel's src_ip/src_ip6 keys: no match across families.
            "ip saddr" if packet.family == "ipv4" => json!(packet.source),
            "ip6 saddr" if packet.family == "ipv6" => json!(packet.source),
            "ip saddr" | "ip6 saddr" => Value::Null,
            key => panic!("unsupported ct {key}"),
        };
    }
    if expression.get("fib").is_some() {
        return json!("unicast");
    }
    if expression.get("socket").is_some() {
        return json!(packet.cgroup);
    }
    if let Some(payload) = expression.get("payload") {
        let protocol = payload["protocol"].as_str().unwrap();
        if (protocol == "ip" && packet.family != "ipv4")
            || (protocol == "ip6" && packet.family != "ipv6")
            || (protocol == "udp" && packet.l4proto != "udp")
            || (protocol == "icmpv6" && packet.l4proto != "ipv6-icmp")
        {
            return Value::Null;
        }
        return match payload["field"].as_str().unwrap() {
            "daddr" => json!(packet.destination),
            "sport" => json!(packet.sport),
            "dport" => json!(packet.dport),
            "hoplimit" => json!(packet.hoplimit),
            "type" => json!(packet.icmp_type),
            key => panic!("unsupported payload {key}"),
        };
    }
    expression.clone()
}

fn equals(left: &Value, right: &Value) -> bool {
    if let Some(range) = right.get("range").and_then(Value::as_array) {
        return left
            .as_u64()
            .is_some_and(|n| n >= range[0].as_u64().unwrap() && n <= range[1].as_u64().unwrap());
    }
    if let (Some(n), Some(text)) = (left.as_u64(), right.as_str()) {
        let resolved = match text {
            "eth0" => Some(2),
            "usb0" => Some(3),
            _ => text.parse::<u64>().ok(),
        };
        if let Some(resolved) = resolved {
            return n == resolved;
        }
    }
    if let Some(set) = right.get("set") {
        return set.as_array().unwrap().iter().any(|r| equals(left, r));
    }
    if let Some(prefix) = right.get("prefix") {
        let net: IpNet = format!("{}/{}", prefix["addr"].as_str().unwrap(), prefix["len"])
            .parse()
            .unwrap();
        return left
            .as_str()
            .and_then(|s| s.parse::<IpAddr>().ok())
            .is_some_and(|ip| net.contains(&ip));
    }
    left == right
}

pub fn execute(document: &Value, chain: &str, packet: &mut Packet) -> &'static str {
    for rule in rules(document, chain) {
        let mut matched = true;
        for expression in rule.as_array().unwrap() {
            if let Some(m) = expression.get("match") {
                let left = value(&m["left"], packet);
                matched = match m["op"].as_str().unwrap() {
                    "==" | "in" => equals(&left, &m["right"]),
                    "!=" => !equals(&left, &m["right"]),
                    ">=" => left.as_u64().unwrap() >= m["right"].as_u64().unwrap(),
                    "<=" => left.as_u64().unwrap() <= m["right"].as_u64().unwrap(),
                    "<" => left.as_u64().unwrap() < m["right"].as_u64().unwrap(),
                    ">" => left.as_u64().unwrap() > m["right"].as_u64().unwrap(),
                    op => panic!("unsupported match {op}"),
                };
                if !matched {
                    break;
                }
            } else if let Some(m) = expression.get("mangle") {
                let n = value(&m["value"], packet).as_u64().unwrap() as u32;
                if m["key"].get("meta").is_some() {
                    packet.mark = n;
                } else {
                    packet.ct_mark = n;
                }
            } else if let Some(goto) = expression.get("goto") {
                return execute(document, goto["target"].as_str().unwrap(), packet);
            } else if let Some(jump) = expression.get("jump") {
                let result = execute(document, jump["target"].as_str().unwrap(), packet);
                if result != "return" {
                    return result;
                }
            } else if expression.get("reject").is_some() {
                return "reject";
            } else if expression.get("drop").is_some() {
                return "drop";
            } else if expression.get("return").is_some() {
                return "return";
            } else if expression.get("masquerade").is_some() {
                packet.translated = true;
                return "return";
            } else {
                assert!(
                    expression.get("counter").is_some(),
                    "unsupported {expression}"
                );
            }
        }
        let _ = matched;
    }
    "return"
}
