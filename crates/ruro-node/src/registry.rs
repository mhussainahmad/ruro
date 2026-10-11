use ruro_graph::{NodeDescriptor, Registry};

use crate::{Builder, Ctx, Node, NodeSpec, Result};

/// Object-safe node interface used by hosts.
pub trait DynNode: Send {
    fn start(&mut self, ctx: &mut Ctx) -> Result;
    fn step(&mut self, ctx: &mut Ctx) -> Result;
    fn stop(&mut self, ctx: &mut Ctx);
    fn set_param(&mut self, name: &str, value: &serde_json::Value) -> Result;
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
}

/// Link-time registration of a node type. Emitted by `#[derive(Node)]`.
pub struct Registration {
    pub descriptor: fn() -> NodeDescriptor,
    pub factory: fn(&mut Builder) -> Result<Box<dyn DynNode>>,
}

inventory::collect!(Registration);

/// Descriptors of every node type linked into this binary.
pub fn registry() -> Registry {
    Registry::new(
        inventory::iter::<Registration>
            .into_iter()
            .map(|r| (r.descriptor)()),
    )
}

/// Finds the registration for a node kind.
pub fn find(kind: &str) -> Option<&'static Registration> {
    inventory::iter::<Registration>
        .into_iter()
        .find(|r| (r.descriptor)().kind == kind)
}
