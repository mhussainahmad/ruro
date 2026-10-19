//! Rust code generation from parsed interface definitions.
//!
//! Every message becomes a struct with public fields, a `Default` that
//! honours the defaults declared in the `.msg` file, constants as associated
//! `const`s, CDR encode/decode impls and a [`RosMessage`](crate::RosMessage)
//! impl carrying the type name and RIHS01 hash.
//!
//! Services and actions become unit structs implementing
//! [`RosService`](crate::RosService) and [`RosAction`](crate::RosAction),
//! next to the messages they are made of (`<Srv>_Request`,
//! `<Srv>_Response`, `<Srv>_Event`; `<Action>_Goal`, `<Action>_SendGoal`,
//! `<Action>_FeedbackMessage` and so on).
//!
//! The output is the body of a module: put it in a file and declare it with
//! `#[rustfmt::skip] mod my_msgs;`. Types from packages that are generated
//! elsewhere (e.g. the ones bundled with this crate) are referenced through
//! the paths given in [`Options::externs`].

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Context, Result, bail};

use crate::hash::type_hash;
use crate::idl::{
    ActionSpec, Array, BaseType, Field, FieldType, Interfaces, MessageSpec, Primitive, ServiceSpec,
    TypeName,
};

/// Code generation settings.
#[derive(Debug, Clone)]
pub struct Options {
    /// Packages to emit.
    pub packages: Vec<String>,
    /// Path of the module the output becomes, e.g. `crate::msgs`.
    pub root: String,
    /// Path of the crate providing the CDR traits, e.g. `ruro_ros2` or `crate`.
    pub runtime: String,
    /// Packages generated elsewhere: package name to module path.
    pub externs: BTreeMap<String, String>,
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "box", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod",
    "move", "mut", "priv", "pub", "ref", "return", "static", "struct", "super", "trait", "true",
    "try", "type", "unsafe", "use", "virtual", "where", "while", "yield",
];

fn ident(name: &str) -> String {
    if KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.to_owned()
    }
}

fn rust_primitive(p: Primitive) -> &'static str {
    match p {
        Primitive::Bool => "bool",
        Primitive::Byte | Primitive::Char | Primitive::Uint8 => "u8",
        Primitive::Int8 => "i8",
        Primitive::Int16 => "i16",
        Primitive::Uint16 => "u16",
        Primitive::Int32 => "i32",
        Primitive::Uint32 => "u32",
        Primitive::Int64 => "i64",
        Primitive::Uint64 => "u64",
        Primitive::Float32 => "f32",
        Primitive::Float64 => "f64",
    }
}

/// CDR reader/writer method for a primitive.
fn method(p: Primitive) -> &'static str {
    rust_primitive(p)
}

struct Gen<'a> {
    interfaces: &'a Interfaces,
    opts: &'a Options,
}

impl Gen<'_> {
    fn type_path(&self, t: &TypeName) -> String {
        let pkg = match self.opts.externs.get(&t.package) {
            Some(p) => p.clone(),
            None => format!("{}::{}", self.opts.root, t.package),
        };
        format!("{pkg}::{}::{}", t.namespace, t.name)
    }

    fn element_type(&self, base: &BaseType) -> String {
        match base {
            BaseType::Primitive(p) => rust_primitive(*p).into(),
            BaseType::String { .. } | BaseType::WString { .. } => "::std::string::String".into(),
            BaseType::Named(t) => self.type_path(t),
        }
    }

    fn field_type(&self, ty: &FieldType) -> String {
        let elem = self.element_type(&ty.base);
        match ty.array {
            Array::None => elem,
            Array::Fixed(n) => format!("[{elem}; {n}]"),
            Array::Bounded(_) | Array::Unbounded => format!("::std::vec::Vec<{elem}>"),
        }
    }

    fn default_expr(&self, f: &Field) -> Result<String> {
        let Some(text) = &f.default else {
            return Ok(match f.ty.array {
                Array::Fixed(_) => {
                    "::std::array::from_fn(|_| ::core::default::Default::default())".into()
                }
                _ => "::core::default::Default::default()".into(),
            });
        };
        let scalar = |v: &str| -> Result<String> { literal(&f.ty.base, v.trim()) };
        match f.ty.array {
            Array::None => {
                let v = scalar(text)?;
                Ok(match f.ty.base {
                    BaseType::String { .. } | BaseType::WString { .. } => {
                        format!("::std::string::String::from({v})")
                    }
                    _ => v,
                })
            }
            _ => {
                let inner = text
                    .trim()
                    .strip_prefix('[')
                    .and_then(|t| t.strip_suffix(']'))
                    .with_context(|| format!("array default `{text}` of `{}`", f.name))?;
                let items = split_items(inner)
                    .iter()
                    .map(|v| {
                        let lit = scalar(v)?;
                        Ok(match f.ty.base {
                            BaseType::String { .. } | BaseType::WString { .. } => {
                                format!("::std::string::String::from({lit})")
                            }
                            _ => lit,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(match f.ty.array {
                    Array::Fixed(_) => format!("[{}]", items.join(", ")),
                    _ => format!("::std::vec![{}]", items.join(", ")),
                })
            }
        }
    }

    fn encode_stmt(&self, f: &Field) -> String {
        let name = ident(&f.name);
        let bytes = matches!(
            f.ty.base,
            BaseType::Primitive(Primitive::Uint8 | Primitive::Byte | Primitive::Char)
        );
        match (&f.ty.base, f.ty.array) {
            (BaseType::Primitive(p), Array::None) => format!("w.{}(self.{name});", method(*p)),
            (BaseType::String { .. }, Array::None) => format!("w.string(&self.{name});"),
            (BaseType::WString { .. }, Array::None) => format!("w.wstring(&self.{name});"),
            // `String` encodes as a `string`, so `wstring` arrays are written here.
            (BaseType::WString { .. }, Array::Fixed(_)) => {
                format!("for s in &self.{name} {{ w.wstring(s); }}")
            }
            (BaseType::WString { .. }, Array::Bounded(_) | Array::Unbounded) => {
                format!("w.len(self.{name}.len()); for s in &self.{name} {{ w.wstring(s); }}")
            }
            (_, Array::Bounded(_) | Array::Unbounded) if bytes => {
                format!("w.byte_seq(&self.{name});")
            }
            _ => format!(
                "{}::cdr::CdrEncode::encode(&self.{name}, w);",
                self.opts.runtime
            ),
        }
    }

    fn decode_expr(&self, f: &Field) -> String {
        let bytes = matches!(
            f.ty.base,
            BaseType::Primitive(Primitive::Uint8 | Primitive::Byte | Primitive::Char)
        );
        match (&f.ty.base, f.ty.array) {
            (BaseType::Primitive(p), Array::None) => format!("r.{}()?", method(*p)),
            (BaseType::String { .. }, Array::None) => "r.string()?".into(),
            (BaseType::WString { .. }, Array::None) => "r.wstring()?".into(),
            (BaseType::WString { .. }, Array::Fixed(_)) => "{ let mut a: [::std::string::String; _] = ::std::array::from_fn(|_| ::std::string::String::new()); for s in a.iter_mut() { *s = r.wstring()?; } a }".into(),
            (BaseType::WString { .. }, Array::Bounded(_) | Array::Unbounded) => "{ let n = r.len(4)?; let mut v = ::std::vec::Vec::with_capacity(n); for _ in 0..n { v.push(r.wstring()?); } v }".into(),
            (_, Array::Bounded(_) | Array::Unbounded) if bytes => "r.byte_seq()?".into(),
            _ => format!("{}::cdr::CdrDecode::decode(r)?", self.opts.runtime),
        }
    }

    fn constant(&self, ty: &str, name: &str, value: &str) -> Result<String> {
        let (rust_ty, lit) = if ty == "string" || ty.starts_with("string<=") {
            (
                "&'static str",
                literal(&BaseType::String { bound: None }, value)?,
            )
        } else {
            let p = Primitive::parse(ty)
                .ok_or_else(|| anyhow::anyhow!("constant {name} has unsupported type {ty}"))?;
            (rust_primitive(p), literal(&BaseType::Primitive(p), value)?)
        };
        Ok(format!("    pub const {name}: {rust_ty} = {lit};\n"))
    }

    fn message(&self, spec: &MessageSpec, out: &mut String) -> Result<()> {
        let rt = &self.opts.runtime;
        let name = &spec.name.name;
        let hash = type_hash(self.interfaces, &spec.name)
            .map_err(|missing| anyhow::anyhow!("{} references unknown {missing}", spec.name))?;
        let _ = writeln!(out, "/// `{}`", spec.name);
        out.push_str("#[derive(Debug, Clone, PartialEq)]\n");
        let _ = writeln!(out, "pub struct {name} {{");
        for f in &spec.fields {
            let _ = writeln!(
                out,
                "    pub {}: {},",
                ident(&f.name),
                self.field_type(&f.ty)
            );
        }
        out.push_str("}\n\n");

        let _ = writeln!(out, "impl ::core::default::Default for {name} {{");
        out.push_str("    fn default() -> Self {\n        Self {\n");
        for f in &spec.fields {
            let _ = writeln!(
                out,
                "            {}: {},",
                ident(&f.name),
                self.default_expr(f)
                    .with_context(|| format!("default of {}.{}", spec.name, f.name))?
            );
        }
        out.push_str("        }\n    }\n}\n\n");

        if !spec.constants.is_empty() {
            let _ = writeln!(out, "impl {name} {{");
            for c in &spec.constants {
                out.push_str(
                    &self
                        .constant(&c.ty, &c.name, &c.value)
                        .with_context(|| format!("in {}", spec.name))?,
                );
            }
            out.push_str("}\n\n");
        }

        let _ = writeln!(out, "impl {rt}::cdr::CdrEncode for {name} {{");
        let w = if spec.fields.is_empty() { "_w" } else { "w" };
        let _ = writeln!(
            out,
            "    fn encode(&self, {w}: &mut {rt}::cdr::CdrWriter) {{"
        );
        if spec.fields.is_empty() {
            // The placeholder member of empty messages.
            out.push_str("        _w.u8(0);\n");
        }
        for f in &spec.fields {
            let _ = writeln!(out, "        {}", self.encode_stmt(f));
        }
        out.push_str("    }\n}\n\n");

        let _ = writeln!(out, "impl {rt}::cdr::CdrDecode for {name} {{");
        let _ = writeln!(
            out,
            "    fn decode(r: &mut {rt}::cdr::CdrReader<'_>) -> ::core::result::Result<Self, {rt}::cdr::CdrError> {{"
        );
        if spec.fields.is_empty() {
            out.push_str("        r.u8()?;\n        Ok(Self {})\n");
        } else {
            out.push_str("        Ok(Self {\n");
            for f in &spec.fields {
                let _ = writeln!(
                    out,
                    "            {}: {},",
                    ident(&f.name),
                    self.decode_expr(f)
                );
            }
            out.push_str("        })\n");
        }
        out.push_str("    }\n}\n\n");

        let _ = writeln!(out, "impl {rt}::RosMessage for {name} {{");
        let _ = writeln!(
            out,
            "    const TYPE_NAME: &'static str = \"{}\";",
            spec.name
        );
        let _ = writeln!(
            out,
            "    const DDS_TYPE_NAME: &'static str = \"{}\";",
            spec.name.dds_name()
        );
        let _ = writeln!(out, "    const TYPE_HASH: &'static str = \"{hash}\";");
        out.push_str("}\n\n");
        Ok(())
    }
}

impl Gen<'_> {
    fn hash(&self, name: &TypeName) -> Result<String> {
        type_hash(self.interfaces, name)
            .map_err(|missing| anyhow::anyhow!("{name} references unknown {missing}"))
    }

    /// A unit struct implementing `RosService`.
    fn service(&self, spec: &ServiceSpec, out: &mut String) -> Result<()> {
        let rt = &self.opts.runtime;
        let name = &spec.name.name;
        let hash = self.hash(&spec.name)?;
        let _ = writeln!(out, "/// `{}`", spec.name);
        out.push_str("#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]\n");
        let _ = writeln!(out, "pub struct {name};\n");
        let _ = writeln!(out, "impl {rt}::RosService for {name} {{");
        let _ = writeln!(
            out,
            "    const TYPE_NAME: &'static str = \"{}\";",
            spec.name
        );
        let _ = writeln!(
            out,
            "    const DDS_TYPE_NAME: &'static str = \"{}\";",
            spec.name.dds_name()
        );
        let _ = writeln!(out, "    const TYPE_HASH: &'static str = \"{hash}\";");
        let _ = writeln!(out, "    type Request = {};", spec.request.name.name);
        let _ = writeln!(out, "    type Response = {};", spec.response.name.name);
        out.push_str("}\n\n");
        Ok(())
    }

    /// A unit struct implementing `RosAction`.
    fn action(&self, spec: &ActionSpec, out: &mut String) -> Result<()> {
        let rt = &self.opts.runtime;
        let n = &spec.name.name;
        let hash = self.hash(&spec.name)?;
        let uuid = self.type_path(&TypeName::msg("unique_identifier_msgs", "UUID"));
        let time = format!("{rt}::msgs::builtin_interfaces::msg::Time");
        let id = format!("{rt}::GoalId");
        let _ = writeln!(out, "/// `{}`", spec.name);
        out.push_str("#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]\n");
        let _ = writeln!(out, "pub struct {n};\n");
        let _ = write!(
            out,
            "impl {rt}::RosAction for {n} {{
    const TYPE_NAME: &'static str = \"{ty}\";
    const TYPE_HASH: &'static str = \"{hash}\";
    type Goal = {n}_Goal;
    type Result = {n}_Result;
    type Feedback = {n}_Feedback;
    type SendGoal = {n}_SendGoal;
    type GetResult = {n}_GetResult;
    type FeedbackMessage = {n}_FeedbackMessage;

    fn send_goal_request(goal_id: {id}, goal: {n}_Goal) -> {n}_SendGoal_Request {{
        {n}_SendGoal_Request {{ goal_id: {uuid} {{ uuid: goal_id }}, goal }}
    }}

    fn split_send_goal_request(request: {n}_SendGoal_Request) -> ({id}, {n}_Goal) {{
        (request.goal_id.uuid, request.goal)
    }}

    fn send_goal_response(accepted: bool, stamp: {time}) -> {n}_SendGoal_Response {{
        {n}_SendGoal_Response {{ accepted, stamp }}
    }}

    fn split_send_goal_response(response: {n}_SendGoal_Response) -> (bool, {time}) {{
        (response.accepted, response.stamp)
    }}

    fn get_result_request(goal_id: {id}) -> {n}_GetResult_Request {{
        {n}_GetResult_Request {{ goal_id: {uuid} {{ uuid: goal_id }} }}
    }}

    fn split_get_result_request(request: {n}_GetResult_Request) -> {id} {{
        request.goal_id.uuid
    }}

    fn get_result_response(status: i8, result: {n}_Result) -> {n}_GetResult_Response {{
        {n}_GetResult_Response {{ status, result }}
    }}

    fn split_get_result_response(response: {n}_GetResult_Response) -> (i8, {n}_Result) {{
        (response.status, response.result)
    }}

    fn feedback_message(goal_id: {id}, feedback: {n}_Feedback) -> {n}_FeedbackMessage {{
        {n}_FeedbackMessage {{ goal_id: {uuid} {{ uuid: goal_id }}, feedback }}
    }}

    fn split_feedback_message(message: {n}_FeedbackMessage) -> ({id}, {n}_Feedback) {{
        (message.goal_id.uuid, message.feedback)
    }}
}}

",
            ty = spec.name,
        );
        Ok(())
    }
}

/// Splits `a, "b, c", d` at top-level commas.
fn split_items(s: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (quote, c) {
            (None, '"' | '\'') => {
                quote = Some(c);
                cur.push(c);
            }
            (Some(q), c) if c == q => {
                quote = None;
                cur.push(c);
            }
            (None, ',') => items.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        items.push(cur);
    }
    items.into_iter().map(|i| i.trim().to_owned()).collect()
}

/// A Rust literal for a default or constant value.
fn literal(base: &BaseType, v: &str) -> Result<String> {
    Ok(match base {
        BaseType::Primitive(Primitive::Bool) => match v.to_ascii_lowercase().as_str() {
            "true" | "1" => "true".into(),
            "false" | "0" => "false".into(),
            _ => bail!("bad bool `{v}`"),
        },
        BaseType::Primitive(p @ (Primitive::Float32 | Primitive::Float64)) => {
            let f: f64 = v.parse().with_context(|| format!("bad number `{v}`"))?;
            format!("{f:?}_{}", rust_primitive(*p))
        }
        BaseType::Primitive(p) => {
            let n: i128 = v.parse().with_context(|| format!("bad integer `{v}`"))?;
            format!("{n}_{}", rust_primitive(*p))
        }
        BaseType::String { .. } | BaseType::WString { .. } => {
            let unquoted = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(v);
            format!("{unquoted:?}")
        }
        BaseType::Named(t) => bail!("messages ({t}) cannot have default values"),
    })
}

/// Generates the module body for `opts.packages`.
pub fn generate(interfaces: &Interfaces, opts: &Options) -> Result<String> {
    let g = Gen { interfaces, opts };
    let mut by_pkg: BTreeMap<&str, BTreeMap<&str, Vec<&MessageSpec>>> = BTreeMap::new();
    for spec in interfaces.messages.values() {
        // Service and action descriptions become marker types below.
        if opts.packages.contains(&spec.name.package) && !interfaces.is_description(&spec.name) {
            by_pkg
                .entry(&spec.name.package)
                .or_default()
                .entry(&spec.name.namespace)
                .or_default()
                .push(spec);
        }
    }
    for p in &opts.packages {
        if !by_pkg.contains_key(p.as_str()) {
            bail!("no interfaces found for package `{p}`");
        }
    }
    let mut out = String::from(
        "// ROS 2 message types, generated by `ruro ros2 gen` from the package\n\
         // interface definitions. Do not edit by hand.\n\n",
    );
    for (pkg, namespaces) in by_pkg {
        let _ = writeln!(out, "pub mod {pkg} {{");
        for (ns, specs) in namespaces {
            let _ = writeln!(out, "pub mod {ns} {{");
            out.push_str(
                "#![allow(non_camel_case_types, non_snake_case, dead_code, clippy::all)]\n\n",
            );
            for spec in specs {
                g.message(spec, &mut out)?;
            }
            let here = |t: &TypeName| t.package == pkg && t.namespace == ns;
            for spec in interfaces.services.values().filter(|s| here(&s.name)) {
                g.service(spec, &mut out)?;
            }
            for spec in interfaces.actions.values().filter(|a| here(&a.name)) {
                g.action(spec, &mut out)?;
            }
            out.push_str("}\n");
        }
        out.push_str("}\n\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idl::parse_message;

    #[test]
    fn literals() {
        let f64t = BaseType::Primitive(Primitive::Float64);
        assert_eq!(literal(&f64t, "1").unwrap(), "1.0_f64");
        assert_eq!(
            literal(&BaseType::Primitive(Primitive::Int8), "-3").unwrap(),
            "-3_i8"
        );
        assert_eq!(
            literal(&BaseType::String { bound: None }, "'a\"b'").unwrap(),
            r#""a\"b""#
        );
        assert!(literal(&BaseType::Primitive(Primitive::Bool), "maybe").is_err());
        assert_eq!(split_items(r#"1, "a,b", 'c'"#), ["1", r#""a,b""#, "'c'"]);
    }

    #[test]
    fn generates_struct_impls_and_defaults() {
        let mut i = Interfaces::default();
        i.add(
            parse_message(
                TypeName::msg("demo", "Thing"),
                "uint8 KIND_A=1\nstring NAME=\"x\"\nfloat64 w 1\nint32[3] grid [1, 2, 3]\nuint8[] blob\nstring type\n",
                "Thing.msg",
            )
            .unwrap(),
        );
        let code = generate(
            &i,
            &Options {
                packages: vec!["demo".into()],
                root: "crate::m".into(),
                runtime: "crate".into(),
                externs: BTreeMap::new(),
            },
        )
        .unwrap();
        assert!(code.contains("pub struct Thing {"));
        assert!(code.contains("pub r#type: ::std::string::String,"));
        assert!(code.contains("w: 1.0_f64,"));
        assert!(code.contains("grid: [1_i32, 2_i32, 3_i32],"));
        assert!(code.contains("pub const KIND_A: u8 = 1_u8;"));
        assert!(code.contains("pub const NAME: &'static str = \"x\";"));
        assert!(code.contains("w.byte_seq(&self.blob);"));
        assert!(code.contains("const TYPE_NAME: &'static str = \"demo/msg/Thing\";"));
        assert!(code.contains("RIHS01_"));
    }
}
