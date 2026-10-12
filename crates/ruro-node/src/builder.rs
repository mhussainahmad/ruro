use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{Context, bail};
use ruro_msg::Message;
use serde_json::Value;

use crate::port::{InputPort, OutputPort};
use crate::{DynNode, ErasedInput, ErasedOutput, Input, Output, ParamValue, Result, Waker};

/// Constructs one node instance: hands out its ports and parameter values.
pub struct Builder<'a> {
    id: &'a str,
    params: &'a BTreeMap<String, Value>,
    depths: HashMap<String, usize>,
    waker: Arc<Waker>,
    inputs: Vec<(String, Arc<dyn ErasedInput>)>,
    outputs: Vec<(String, Arc<dyn ErasedOutput>)>,
}

/// A constructed node with its type-erased ports, ready to be wired.
pub struct BuiltNode {
    pub node: Box<dyn DynNode>,
    pub waker: Arc<Waker>,
    pub inputs: Vec<(String, Arc<dyn ErasedInput>)>,
    pub outputs: Vec<(String, Arc<dyn ErasedOutput>)>,
}

impl<'a> Builder<'a> {
    /// `depths` maps input port names to their queue depth (default 1).
    pub fn new(
        id: &'a str,
        params: &'a BTreeMap<String, Value>,
        depths: HashMap<String, usize>,
    ) -> Self {
        Self {
            id,
            params,
            depths,
            waker: Arc::new(Waker::new()),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    pub fn id(&self) -> &str {
        self.id
    }

    pub fn input<T: Message>(&mut self, name: &str) -> Result<Input<T>> {
        if self.inputs.iter().any(|(n, _)| n == name) {
            bail!("input `{name}` declared twice");
        }
        let depth = self.depths.get(name).copied().unwrap_or(1);
        let port = Arc::new(InputPort::<T>::new(depth, self.waker.clone()));
        self.inputs.push((name.to_owned(), port.clone()));
        Ok(Input::new(port))
    }

    pub fn output<T: Message>(&mut self, name: &str) -> Result<Output<T>> {
        if self.outputs.iter().any(|(n, _)| n == name) {
            bail!("output `{name}` declared twice");
        }
        let port = Arc::new(OutputPort::<T>::new());
        self.outputs.push((name.to_owned(), port.clone()));
        Ok(Output::new(port))
    }

    /// The parameter value the graph sets, as JSON, without conversion. For
    /// nodes whose parameter types are only known at run time.
    pub fn raw_param(&self, name: &str) -> Option<&Value> {
        self.params.get(name)
    }

    /// Reads a parameter from the graph, falling back to `default`.
    pub fn param<T: ParamValue>(&self, name: &str, default: T) -> Result<T> {
        match self.params.get(name) {
            Some(v) => T::from_json(v).with_context(|| format!("parameter `{name}`")),
            None => Ok(default),
        }
    }

    pub fn finish(self, node: Box<dyn DynNode>) -> BuiltNode {
        BuiltNode {
            node,
            waker: self.waker,
            inputs: self.inputs,
            outputs: self.outputs,
        }
    }
}
