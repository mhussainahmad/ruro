use std::sync::Arc;

use parking_lot::RwLock;
use ruro_graph::{NodeDescriptor, Registry};

use crate::{Builder, Ctx, Node, NodeSpec, Result};

/// Object-safe node interface used by hosts.
pub trait DynNode: Send {
    fn start(&mut self, ctx: &mut Ctx) -> Result;
    fn step(&mut self, ctx: &mut Ctx) -> Result;
    fn stop(&mut self, ctx: &mut Ctx);
    fn set_param(&mut self, name: &str, value: &serde_json::Value) -> Result;
    /// See [`Node::on_estop`].
    fn on_estop(&mut self, _ctx: &mut Ctx) {}
}

impl<T: Node + NodeSpec> DynNode for T {
    fn start(&mut self, ctx: &mut Ctx) -> Result {
        Node::start(self, ctx)
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        Node::step(self, ctx)
    }

    fn stop(&mut self, ctx: &mut Ctx) {
        Node::stop(self, ctx)
    }

    fn set_param(&mut self, name: &str, value: &serde_json::Value) -> Result {
        NodeSpec::set_param(self, name, value)
    }

    fn on_estop(&mut self, ctx: &mut Ctx) {
        Node::on_estop(self, ctx)
    }
}

/// Link-time registration of a node type. Emitted by `#[derive(Node)]`.
pub struct Registration {
    pub descriptor: fn() -> NodeDescriptor,
    pub factory: fn(&mut Builder) -> Result<Box<dyn DynNode>>,
}

inventory::collect!(Registration);

/// Constructs a node registered at run time.
pub type DynFactory = Arc<dyn Fn(&mut Builder) -> Result<Box<dyn DynNode>> + Send + Sync>;

/// Node types registered at run time, e.g. by a language binding that
/// discovers nodes when the host starts.
static DYNAMIC: RwLock<Vec<(NodeDescriptor, DynFactory)>> = RwLock::new(Vec::new());

/// Registers a node type at run time. A later registration of the same kind replaces it.
pub fn register_dynamic(descriptor: NodeDescriptor, factory: DynFactory) {
    let mut d = DYNAMIC.write();
    d.retain(|(existing, _)| existing.kind != descriptor.kind);
    d.push((descriptor, factory));
}

/// Descriptors of every node type linked into this binary or registered at run time.
pub fn registry() -> Registry {
    let linked = inventory::iter::<Registration>
        .into_iter()
        .map(|r| (r.descriptor)());
    let dynamic: Vec<NodeDescriptor> = DYNAMIC.read().iter().map(|(d, _)| d.clone()).collect();
    let mut registry = Registry::new(linked.chain(dynamic));
    let schemas = ruro_msg::schema::registered();
    let port_types: Vec<String> = registry
        .iter()
        .flat_map(|d| d.inputs.iter().chain(&d.outputs))
        .map(|p| p.ty.clone())
        .collect();
    for ty in port_types {
        if let Some(s) = schemas.get(ty.as_str()) {
            registry.schema_hashes.insert(ty, s.hash.clone());
        }
    }
    registry
}

/// Finds the registration for a node kind.
pub fn find(kind: &str) -> Option<&'static Registration> {
    inventory::iter::<Registration>
        .into_iter()
        .find(|r| (r.descriptor)().kind == kind)
}

/// Builds a node of any registered kind, linked or dynamic.
pub fn build(kind: &str, b: &mut Builder) -> Option<Result<Box<dyn DynNode>>> {
    if let Some(r) = find(kind) {
        return Some((r.factory)(b));
    }
    let factory = DYNAMIC
        .read()
        .iter()
        .find(|(d, _)| d.kind == kind)
        .map(|(_, f)| f.clone())?;
    Some(factory(b))
}

/// Descriptor of any registered kind, linked or dynamic.
pub fn descriptor(kind: &str) -> Option<NodeDescriptor> {
    find(kind).map(|r| (r.descriptor)()).or_else(|| {
        DYNAMIC
            .read()
            .iter()
            .find(|(d, _)| d.kind == kind)
            .map(|(d, _)| d.clone())
    })
}
