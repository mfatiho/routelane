//! Frozen pre-selective NFT renderer (0ec1661), ownership proof only.
//! Never edit this to follow modern policy; never use it to activate routing.
use super::legacy_journal_v2::LegacyPlanV2 as KernelPlan;
use super::{
    CompiledAction, CompiledRule, CompiledTarget, FamilyExit, IpFamily, LaneAvailability, LanePlan,
    MARK_BASE, MARK_MAX,
};
use ipnet::IpNet;
use serde_json::{json, Value};
const POLICY_SEAL_PREFIX: &str = "rules_v2_";

pub(super) fn normalize_nft_document(value: &mut Value) {
    // nft 1.0.2 lists each chain followed by its rules, newer nft all chains
    // first. Neither placement has meaning; rule order within a chain does,
    // and a stable partition keeps it.
    if let Some(objects) = value.get_mut("nftables").and_then(Value::as_array_mut) {
        let (mut ordered, rules): (Vec<Value>, Vec<Value>) = std::mem::take(objects)
            .into_iter()
            .partition(|object| object.get("rule").is_none());
        ordered.extend(rules);
        *objects = ordered;
    }
    match value {
        Value::Object(object) => {
            object.remove("metainfo");
            object.remove("handle");
            if object.get("counter").is_some_and(|counter| {
                counter
                    .as_object()
                    .is_some_and(|c| c.contains_key("packets") || c.contains_key("bytes"))
            }) {
                object.insert("counter".to_owned(), Value::Null);
            }
            for nested in object.values_mut() {
                normalize_nft_document(nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                normalize_nft_document(nested);
            }
        }
        _ => {}
    }
}

fn content_seal(document: &Value) -> String {
    let objects = document["nftables"]
        .as_array()
        .expect("internal nft document");
    let mut payload = json!({"nftables": objects.iter().filter(|object| {
        !object.as_object().is_some_and(|o|o.is_empty()) && object.get("metainfo").is_none()
            && object["chain"]["name"] != "guard" && object["rule"]["chain"] != "guard"
            && !object["chain"]["name"].as_str().is_some_and(|n|n.starts_with(POLICY_SEAL_PREFIX))
    }).cloned().collect::<Vec<_>>()});
    normalize_nft_document(&mut payload);
    blake3::hash(&serde_json::to_vec(&payload).expect("encode canonical nft policy"))
        .to_hex()
        .to_string()
}

struct Chain {
    object: Value,
    rules: Vec<Value>,
}

impl Chain {
    fn regular(name: &str) -> Self {
        Self {
            object: json!({"chain":{"family":"inet","table":"routelane","name":name}}),
            rules: vec![],
        }
    }
    fn base(name: &str, kind: &str, hook: &str, priority: i32) -> Self {
        Self {
            object: json!({"chain":{"family":"inet","table":"routelane","name":name,
            "type":kind,"hook":hook,"prio":priority,"policy":"accept"}}),
            rules: vec![],
        }
    }
    fn rule(&mut self, expr: Vec<Value>) {
        self.rules
            .push(json!({"rule":{"family":"inet","table":"routelane",
            "chain":self.object["chain"]["name"],"expr":expr}}));
    }
}

fn meta(key: &str) -> Value {
    json!({"meta":{"key":key}})
}
fn ct(key: &str) -> Value {
    json!({"ct":{"key":key}})
}
fn matches(left: Value, op: &str, right: Value) -> Value {
    json!({"match":{"left":left,"op":op,"right":right}})
}
fn equal(left: Value, right: impl Into<Value>) -> Value {
    matches(left, "==", right.into())
}
fn family(f: IpFamily) -> Value {
    equal(
        meta("nfproto"),
        if f == IpFamily::V4 { "ipv4" } else { "ipv6" },
    )
}
fn protocol(f: IpFamily) -> &'static str {
    if f == IpFamily::V4 {
        "ip"
    } else {
        "ip6"
    }
}
fn suffix(f: IpFamily) -> &'static str {
    if f == IpFamily::V4 {
        "4"
    } else {
        "6"
    }
}
fn payload(protocol: &str, field: &str) -> Value {
    json!({"payload":{"protocol":protocol,"field":field}})
}
fn mangle(key: Value, value: Value) -> Value {
    json!({"mangle":{"key":key,"value":value}})
}
fn ret() -> Value {
    json!({"return":null})
}
fn reject(f: Option<IpFamily>) -> Value {
    let kind = match f {
        Some(IpFamily::V4) => "icmp",
        Some(IpFamily::V6) => "icmpv6",
        None => "icmpx",
    };
    json!({"reject":{"type":kind,"expr":"port-unreachable"}})
}
fn goto(name: &str) -> Value {
    json!({"goto":{"target":name}})
}
fn owned(key: Value) -> Vec<Value> {
    vec![equal(key, json!({"range":[MARK_BASE,MARK_MAX]}))]
}
fn network(n: &IpNet) -> Value {
    if n.prefix_len() == if n.addr().is_ipv4() { 32 } else { 128 } {
        json!(n.addr().to_string())
    } else {
        json!({"prefix":{"addr":n.network().to_string(),"len":n.prefix_len()}})
    }
}
fn destination(n: &IpNet) -> Value {
    equal(
        payload(if n.addr().is_ipv4() { "ip" } else { "ip6" }, "daddr"),
        network(n),
    )
}
fn ready(lane: &LanePlan, f: IpFamily) -> bool {
    lane.availability == LaneAvailability::Available
        && lane.identity.as_ref().is_some_and(|identity| {
            identity.ifindex > 0
                && identity.ifindex <= i32::MAX as u32
                && identity.name == lane.interface
        })
        && match f {
            IpFamily::V4 => lane.route_v4.is_some() && !lane.source_v4.is_empty(),
            IpFamily::V6 => lane.route_v6.is_some() && !lane.source_v6.is_empty(),
        }
}
fn selection_name(f: IpFamily, mark: u32) -> String {
    format!("select{}_{mark:08x}", suffix(f))
}
fn restore_name(f: IpFamily) -> String {
    format!("restore{}", suffix(f))
}

pub(super) fn state_chain_name(plan: &KernelPlan) -> String {
    marker_name("state", plan)
}
pub(super) fn transaction_chain_name(plan: &KernelPlan) -> String {
    marker_name("transaction", plan)
}
fn marker_name(kind: &str, plan: &KernelPlan) -> String {
    let encoded = serde_json::to_vec(&plan.content_hash()).expect("encode policy hash");
    format!(
        "{kind}_g{}_{:.16}",
        plan.generation,
        blake3::hash(&encoded).to_hex()
    )
}

/// Preserve only host-local and on-link control traffic before explicit policy.
fn controls(chain: &mut Chain, networks: &[IpNet]) {
    chain.rule(vec![equal(meta("oifname"), "lo"), ret()]);
    chain.rule(vec![
        equal(json!({"fib":{"result":"type","flags":["daddr"]}}), "local"),
        ret(),
    ]);
    chain.rule(vec![
        equal(payload("udp", "sport"), 68),
        equal(payload("udp", "dport"), 67),
        equal(payload("ip", "daddr"), "255.255.255.255"),
        ret(),
    ]);
    for subnet in networks.iter().filter(|n| n.addr().is_ipv4()) {
        chain.rule(vec![
            equal(payload("udp", "sport"), 68),
            equal(payload("udp", "dport"), 67),
            destination(subnet),
            ret(),
        ]);
    }
    for subnet in ["224.0.0.0/24", "ff02::/16"] {
        chain.rule(vec![
            destination(&subnet.parse().expect("constant multicast prefix")),
            ret(),
        ]);
    }
    // RFC 4861 ND is link-local control even when its unicast destination is a
    // global on-link address. A hop limit of 255 keeps this exemption narrow.
    chain.rule(vec![equal(payload("ip6","hoplimit"),255),equal(payload("icmpv6","type"),
            json!({"set":["nd-router-solicit","nd-router-advert","nd-neighbor-solicit","nd-neighbor-advert","nd-redirect"]})),ret()]);
    chain.rule(vec![
        equal(payload("udp", "sport"), 546),
        equal(payload("udp", "dport"), 547),
        destination(&"fe80::/10".parse().expect("constant link-local prefix")),
        ret(),
    ]);
}

fn preserve_unmanaged_sessions(chain: &mut Chain) {
    chain.rule(vec![
        equal(ct("mark"), 0),
        equal(ct("state"), json!({"set":["established","related"]})),
        ret(),
    ]);
    chain.rule(vec![
        equal(ct("mark"), 0),
        equal(ct("direction"), "reply"),
        ret(),
    ]);
}

fn source_proof(plan: &KernelPlan, f: IpFamily) -> Option<Value> {
    let mut addresses = plan
        .local_addresses
        .iter()
        .copied()
        .filter(|a| a.is_ipv4() == (f == IpFamily::V4))
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return None;
    }
    let addresses = addresses
        .into_iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>();
    let right = if addresses.len() == 1 {
        json!(addresses[0])
    } else {
        json!({"set":addresses})
    };
    // nft 1.0.9 canonical dump spells the family in the CT key, not a separate
    // family attribute. Use its round-trippable representation.
    Some(equal(
        json!({"ct":{"key":format!("{} saddr",protocol(f)),"dir":"original"}}),
        right,
    ))
}

fn primary_mark(plan: &KernelPlan, f: IpFamily) -> Option<u32> {
    let exit = if f == IpFamily::V4 {
        &plan.primary.effective.v4
    } else {
        &plan.primary.effective.v6
    };
    match exit {
        FamilyExit::Lane(id) => plan
            .lanes
            .iter()
            .find(|l| &l.id == id && ready(l, f))
            .map(|l| l.mark),
        FamilyExit::Blocked => None,
    }
}

pub(super) fn explicit_rule(plan: &KernelPlan, rule: &CompiledRule) -> Vec<Value> {
    // IP payload predicates already encode their family. nft removes a
    // redundant nfproto dependency when dumping them; emit the canonical form.
    let mut expr = if matches!(rule.target, CompiledTarget::Network(_)) {
        vec![]
    } else {
        vec![family(rule.family)]
    };
    match &rule.target {
        CompiledTarget::Any => {}
        CompiledTarget::Network(n) => expr.push(destination(n)),
        CompiledTarget::Cgroup(path) => expr.push(equal(
            json!({"socket":{"key":"cgroupv2","level":2}}),
            path.clone(),
        )),
    }
    expr.push(json!({"counter":null}));
    let mark = match rule.action {
        CompiledAction::Mark(m) => Some(m),
        CompiledAction::Direct => primary_mark(plan, rule.family),
        CompiledAction::Block => None,
    };
    expr.push(match mark {
        Some(m) if plan.lanes.iter().any(|l| l.mark == m) => goto(&selection_name(rule.family, m)),
        _ => reject(Some(rule.family)),
    });
    expr
}

pub fn render_nft_document(
    active: Option<&KernelPlan>,
    pending: Option<&KernelPlan>,
    guard: bool,
) -> Value {
    let mut chains = Vec::new();
    if let Some(p) = active {
        chains.push(Chain::regular(&state_chain_name(p)));
    }
    if let Some(p) = pending {
        chains.push(Chain::regular(&transaction_chain_name(p)));
    }
    if let Some(plan) = active {
        for f in [IpFamily::V4, IpFamily::V6] {
            let mut restore = Chain::regular(&restore_name(f));
            for lane in plan.lanes.iter().filter(|l| ready(l, f)) {
                restore.rule(vec![
                    equal(ct("mark"), lane.mark),
                    mangle(meta("mark"), ct("mark")),
                    ret(),
                ]);
            }
            restore.rule(vec![reject(None)]);
            chains.push(restore);
            for lane in &plan.lanes {
                let mut select = Chain::regular(&selection_name(f, lane.mark));
                select.rule(vec![
                    matches(ct("mark"), "!=", json!(0)),
                    goto(&restore_name(f)),
                ]);
                preserve_unmanaged_sessions(&mut select);
                if ready(lane, f) {
                    select.rule(vec![
                        matches(ct("state"), "in", json!("new")),
                        equal(ct("direction"), "original"),
                        mangle(meta("mark"), json!(lane.mark)),
                        mangle(ct("mark"), meta("mark")),
                        ret(),
                    ]);
                }
                select.rule(vec![reject(None)]);
                chains.push(select);
            }
        }
        let mut output = Chain::base("output", "route", "output", -150);
        controls(&mut output, &plan.local_networks);
        // Never erase VPN/foreign policy marks. Fail explicitly rather than
        // silently presenting those packets as RouteLane-managed internet.
        for key in [meta("mark"), ct("mark")] {
            output.rule(vec![
                matches(key.clone(), "!=", json!(0)),
                matches(key.clone(), "<", json!(MARK_BASE)),
                reject(None),
            ]);
            output.rule(vec![matches(key, ">", json!(MARK_MAX)), reject(None)]);
        }
        for rule in &plan.rules {
            output.rule(explicit_rule(plan, rule));
        }
        for f in [IpFamily::V4, IpFamily::V6] {
            let mut expr = vec![family(f)];
            expr.extend(owned(ct("mark")));
            expr.push(goto(&restore_name(f)));
            output.rule(expr);
        }
        preserve_unmanaged_sessions(&mut output);
        for n in &plan.local_networks {
            output.rule(vec![destination(n), ret()]);
        }
        for f in [IpFamily::V4, IpFamily::V6] {
            output.rule(vec![
                family(f),
                json!({"counter":null}),
                match primary_mark(plan, f) {
                    Some(mark) => goto(&selection_name(f, mark)),
                    None => reject(Some(f)),
                },
            ]);
        }
        chains.push(output);

        let mut reply = Chain::base("prerouting", "filter", "prerouting", -150);
        let mut egress = Chain::base("egress_guard", "filter", "postrouting", 99);
        egress.rule(vec![matches(meta("iif"), "!=", json!(0)), ret()]);
        controls(&mut egress, &plan.local_networks);
        let mut nat = Chain::base("source_nat", "nat", "postrouting", 100);
        for f in [IpFamily::V4, IpFamily::V6] {
            let Some(source) = source_proof(plan, f) else {
                continue;
            };
            for lane in plan.lanes.iter().filter(|l| ready(l, f)) {
                // Use the observed index itself; reloading an older guarded
                // plan must never resolve a reused name to a new device.
                let index = lane.identity.as_ref().expect("ready lane identity").ifindex;
                // `source` loads a family-specific conntrack key, so no nfproto
                // dependency: nft 1.1.6 dumps that dependency inconsistently.
                reply.rule(vec![
                    equal(meta("iif"), index),
                    equal(meta("iifname"), lane.interface.clone()),
                    equal(ct("direction"), "reply"),
                    source.clone(),
                    equal(ct("mark"), lane.mark),
                    mangle(meta("mark"), ct("mark")),
                    ret(),
                ]);
                let scope = vec![
                    equal(meta("iif"), 0),
                    equal(meta("oif"), index),
                    equal(meta("oifname"), lane.interface.clone()),
                    equal(ct("direction"), "original"),
                    source.clone(),
                    equal(ct("mark"), lane.mark),
                    equal(meta("mark"), lane.mark),
                ];
                let mut guard_expr = scope.clone();
                guard_expr.push(ret());
                egress.rule(guard_expr);
                let mut nat_expr = scope;
                nat_expr.push(json!({"counter":null}));
                nat_expr.push(json!({"masquerade":null}));
                nat.rule(nat_expr);
            }
        }
        // A deleted policy route must not send an already marked local flow
        // through the main default in the event-loop refresh interval.
        let mut invalid = owned(meta("mark"));
        invalid.push(json!({"drop":null}));
        egress.rule(invalid);
        chains.extend([reply, egress, nat]);
    }
    if guard {
        let mut chain = Chain::base("guard", "route", "output", -149);
        let mut networks = active
            .into_iter()
            .chain(pending)
            .flat_map(|p| p.local_networks.iter().copied())
            .collect::<Vec<_>>();
        networks.sort();
        networks.dedup();
        controls(&mut chain, &networks);
        for n in networks {
            chain.rule(vec![destination(&n), ret()]);
        }
        chain.rule(vec![reject(None)]);
        chains.push(chain);
    }
    // nft list emits all chain definitions before all rules. Keep that exact
    // shape both for add batches (forward targets exist) and ownership proof.
    let mut objects = vec![
        json!({}),
        json!({"table":{"family":"inet","name":"routelane"}}),
    ];
    objects.extend(chains.iter().map(|c| c.object.clone()));
    objects.extend(chains.into_iter().flat_map(|c| c.rules));
    let seal = format!(
        "{POLICY_SEAL_PREFIX}{}",
        content_seal(&json!({"nftables":objects}))
    );
    objects.insert(2, Chain::regular(&seal).object);
    json!({"nftables":objects})
}
