pub mod backend;
pub mod daemon;
pub mod domain;
pub mod ipc;

#[allow(dead_code)]
#[path = "routing/resolver.rs"]
pub(crate) mod exact_domain_resolver;
