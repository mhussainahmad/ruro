//! Graph specification, node descriptors and validation.
//!
//! A graph is a set of named node instances plus the wires between their ports.
//! It is plain data: the studio and the CLI both read and write the same YAML,
//! and the daemon validates it against the node descriptors exported by hosts.

pub mod composite;
mod descriptor;
mod spec;
mod validate;

pub use composite::{COMPOSITE, GRAPH_PREFIX, composite_descriptor, is_composite_kind};
pub use descriptor::{NodeDescriptor, ParamDesc, ParamKind, PortDesc, Registry};
pub use spec::{
    DEFAULT_GROUP, Graph, GraphError, GroupConfig, NodeInstance, PortList, PortRef, Realtime,
    RestartPolicy, Wire,
};
pub use validate::{Issue, validate, validate_in};

/// Returns true if `s` is usable as a node, port or group identifier.
///
/// Identifiers become part of shared-memory service names and key expressions,
/// so they are restricted to `[A-Za-z0-9_-]`.
pub fn is_valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
