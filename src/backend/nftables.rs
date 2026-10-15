use super::{render_nft_document, KernelPlan};
use serde_json::Value;
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableMode {
    Create,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftRenderError {
    InvalidInterfaceName,
    UnsupportedPolicyExpression,
}

impl std::fmt::Display for NftRenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInterfaceName => "invalid kernel interface name",
            Self::UnsupportedPolicyExpression => "unsupported internal nftables policy expression",
        })
    }
}
impl std::error::Error for NftRenderError {}

pub fn render_ruleset(plan: &KernelPlan, mode: TableMode) -> Result<String, NftRenderError> {
    if plan
        .lanes
        .iter()
        .any(|l| !is_kernel_interface_name(&l.interface))
    {
        return Err(NftRenderError::InvalidInterfaceName);
    }
    let document = render_nft_document(Some(plan), None, false);
    let mut text = String::new();
    if mode == TableMode::Replace {
        text.push_str("delete table inet routelane\n");
    }
    text.push_str("add table inet routelane\n");
    for object in document["nftables"]
        .as_array()
        .ok_or(NftRenderError::UnsupportedPolicyExpression)?
    {
        if let Some(chain) = object.get("chain") {
            write!(text, "add chain inet routelane {}", string(&chain["name"])?).unwrap();
            if chain.get("hook").is_some() {
                write!(
                    text,
                    " {{ type {} hook {} priority {}; policy accept; }}",
                    string(&chain["type"])?,
                    string(&chain["hook"])?,
                    chain["prio"]
                )
                .unwrap();
            }
            text.push('\n');
        } else if let Some(rule) = object.get("rule") {
            write!(text, "add rule inet routelane {}", string(&rule["chain"])?).unwrap();
            for expression in rule["expr"]
                .as_array()
                .ok_or(NftRenderError::UnsupportedPolicyExpression)?
            {
                write!(text, " {}", statement(expression)?).unwrap();
            }
            text.push('\n');
        }
    }
    Ok(text)
}

fn string(v: &Value) -> Result<&str, NftRenderError> {
    v.as_str()
        .ok_or(NftRenderError::UnsupportedPolicyExpression)
}
fn value(v: &Value) -> Result<String, NftRenderError> {
    if v.is_string() || v.is_number() {
        return Ok(v.to_string());
    }
    if let Some(m) = v.get("meta") {
        return Ok(format!("meta {}", string(&m["key"])?));
    }
    if let Some(c) = v.get("ct") {
        return Ok(format!(
            "ct {}{}",
            c.get("dir")
                .map(|d| format!("{} ", d.as_str().unwrap_or_default()))
                .unwrap_or_default(),
            string(&c["key"])?
        ));
    }
    if let Some(p) = v.get("payload") {
        return Ok(format!(
            "{} {}",
            string(&p["protocol"])?,
            string(&p["field"])?
        ));
    }
    if let Some(s) = v.get("socket") {
        return Ok(format!(
            "socket {} level {}",
            string(&s["key"])?,
            s["level"]
        ));
    }
    if let Some(p) = v.get("prefix") {
        return Ok(format!("{}/{}", string(&p["addr"])?, p["len"]));
    }
    if let Some(r) = v.get("range").and_then(Value::as_array) {
        if r.len() == 2 {
            return Ok(format!("{}-{}", value(&r[0])?, value(&r[1])?));
        }
    }
    if let Some(set) = v.get("set") {
        return Ok(format!(
            "{{ {} }}",
            set.as_array()
                .ok_or(NftRenderError::UnsupportedPolicyExpression)?
                .iter()
                .map(value)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ));
    }
    if v.get("fib").is_some() {
        return Ok("fib daddr type".to_owned());
    }
    Err(NftRenderError::UnsupportedPolicyExpression)
}
fn statement(v: &Value) -> Result<String, NftRenderError> {
    if let Some(m) = v.get("match") {
        let operator = string(&m["op"])?;
        return Ok(format!(
            "{} {}{}",
            value(&m["left"])?,
            if matches!(operator, "==" | "in") {
                String::new()
            } else {
                format!("{operator} ")
            },
            value(&m["right"])?
        ));
    }
    if let Some(m) = v.get("mangle") {
        return Ok(format!("{} set {}", value(&m["key"])?, value(&m["value"])?));
    }
    for name in ["goto", "jump"] {
        if let Some(g) = v.get(name) {
            return Ok(format!("{name} {}", string(&g["target"])?));
        }
    }
    if let Some(r) = v.get("reject") {
        return Ok(format!(
            "reject with {} type {}",
            string(&r["type"])?,
            string(&r["expr"])?
        ));
    }
    for name in ["counter", "return", "drop", "masquerade"] {
        if v.get(name).is_some() {
            return Ok(name.to_owned());
        }
    }
    Err(NftRenderError::UnsupportedPolicyExpression)
}

fn is_kernel_interface_name(interface: &str) -> bool {
    !interface.is_empty()
        && interface.len() <= 15
        && interface
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
