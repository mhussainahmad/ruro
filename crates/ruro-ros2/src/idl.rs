//! Parser for ROS 2 `.msg` and `.srv` interface definitions.
//!
//! Follows `rosidl_adapter`: one field or constant per line, `#` starts a
//! comment, `type name [default]` declares a field and `TYPE NAME=value` a
//! constant. Types are primitives, `string`/`wstring` (optionally bounded with
//! `<=N`), or messages written `pkg/Type` (or just `Type` within the same
//! package), each optionally followed by `[]`, `[N]` or `[<=N]`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use thiserror::Error;

/// A malformed interface definition.
#[derive(Debug, Error, PartialEq)]
#[error("{file}:{line}: {msg}")]
pub struct ParseError {
    pub file: String,
    pub line: usize,
    pub msg: String,
}

/// A primitive type, as named in `.msg` files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primitive {
    Bool,
    /// `byte`: an opaque octet.
    Byte,
    /// `char`: an alias of `uint8` in ROS 2.
    Char,
    Int8,
    Uint8,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Int64,
    Uint64,
    Float32,
    Float64,
}

impl Primitive {
    /// Parses a `.msg` primitive type name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "bool" => Self::Bool,
            "byte" => Self::Byte,
            "char" => Self::Char,
            "int8" => Self::Int8,
            "uint8" => Self::Uint8,
            "int16" => Self::Int16,
            "uint16" => Self::Uint16,
            "int32" => Self::Int32,
            "uint32" => Self::Uint32,
            "int64" => Self::Int64,
            "uint64" => Self::Uint64,
            "float32" => Self::Float32,
            "float64" => Self::Float64,
            _ => return None,
        })
    }

    /// Size in bytes on the wire (also the CDR alignment).
    pub fn size(self) -> usize {
        match self {
            Self::Bool | Self::Byte | Self::Char | Self::Int8 | Self::Uint8 => 1,
            Self::Int16 | Self::Uint16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Int64 | Self::Uint64 | Self::Float64 => 8,
        }
    }

    /// The `.msg` spelling.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Byte => "byte",
            Self::Char => "char",
            Self::Int8 => "int8",
            Self::Uint8 => "uint8",
            Self::Int16 => "int16",
            Self::Uint16 => "uint16",
            Self::Int32 => "int32",
            Self::Uint32 => "uint32",
            Self::Int64 => "int64",
            Self::Uint64 => "uint64",
            Self::Float32 => "float32",
            Self::Float64 => "float64",
        }
    }
}

/// The element type of a field, before any array suffix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BaseType {
    Primitive(Primitive),
    /// `string` or `string<=N`.
    String {
        bound: Option<usize>,
    },
    /// `wstring` or `wstring<=N`.
    WString {
        bound: Option<usize>,
    },
    /// Another message.
    Named(TypeName),
}

/// Array-ness of a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Array {
    /// A single value.
    None,
    /// `T[N]`: exactly N values, no length on the wire.
    Fixed(usize),
    /// `T[<=N]`: up to N values, length-prefixed.
    Bounded(usize),
    /// `T[]`: any number of values, length-prefixed.
    Unbounded,
}

/// A field type: element type plus array-ness.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldType {
    pub base: BaseType,
    pub array: Array,
}

/// A message name qualified by its package, e.g. `geometry_msgs/msg/Twist`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeName {
    pub package: String,
    /// `msg` or `srv`.
    pub namespace: String,
    pub name: String,
}

impl TypeName {
    /// A message type in `package`.
    pub fn msg(package: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            package: package.into(),
            namespace: "msg".into(),
            name: name.into(),
        }
    }

    /// Parses `pkg/msg/Type` or `pkg/Type`.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('/').collect();
        match parts.as_slice() {
            [pkg, ns, name] => Some(Self {
                package: (*pkg).into(),
                namespace: (*ns).into(),
                name: (*name).into(),
            }),
            [pkg, name] => Some(Self::msg(*pkg, *name)),
            _ => None,
        }
    }

    /// The type name used on DDS and Zenoh, e.g. `std_msgs::msg::dds_::String_`.
    pub fn dds_name(&self) -> String {
        format!("{}::{}::dds_::{}_", self.package, self.namespace, self.name)
    }

    /// Parses a DDS type name back into a ROS type name.
    pub fn from_dds(s: &str) -> Option<Self> {
        let mut parts = s.split("::");
        let package = parts.next()?;
        let namespace = parts.next()?;
        if parts.next()? != "dds_" {
            return None;
        }
        let name = parts.next()?.strip_suffix('_')?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            package: package.into(),
            namespace: namespace.into(),
            name: name.into(),
        })
    }
}

impl fmt::Display for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.package, self.namespace, self.name)
    }
}

/// A field declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: FieldType,
    /// Default value as written in the file.
    pub default: Option<String>,
}

/// A constant declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Constant {
    /// Primitive or `string`.
    pub ty: String,
    pub name: String,
    pub value: String,
}

/// A parsed message.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageSpec {
    pub name: TypeName,
    pub fields: Vec<Field>,
    pub constants: Vec<Constant>,
}

/// A parsed service: request and response messages named `<Srv>_Request`
/// and `<Srv>_Response`.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceSpec {
    pub name: TypeName,
    pub request: MessageSpec,
    pub response: MessageSpec,
}

fn parse_type(s: &str, package: &str) -> Result<FieldType, String> {
    let (elem, array) = match s.find('[') {
        Some(i) => {
            let suffix = s[i..]
                .strip_prefix('[')
                .and_then(|r| r.strip_suffix(']'))
                .ok_or_else(|| format!("bad array type `{s}`"))?;
            let array = if suffix.is_empty() {
                Array::Unbounded
            } else if let Some(n) = suffix.strip_prefix("<=") {
                Array::Bounded(n.parse().map_err(|_| format!("bad array bound in `{s}`"))?)
            } else {
                Array::Fixed(
                    suffix
                        .parse()
                        .map_err(|_| format!("bad array size in `{s}`"))?,
                )
            };
            (&s[..i], array)
        }
        None => (s, Array::None),
    };
    let bounded = |rest: &str| -> Result<Option<usize>, String> {
        if rest.is_empty() {
            Ok(None)
        } else {
            rest.strip_prefix("<=")
                .and_then(|n| n.parse().ok())
                .map(Some)
                .ok_or_else(|| format!("bad string bound in `{s}`"))
        }
    };
    let base = if let Some(p) = Primitive::parse(elem) {
        BaseType::Primitive(p)
    } else if let Some(rest) = elem.strip_prefix("wstring") {
        BaseType::WString {
            bound: bounded(rest)?,
        }
    } else if let Some(rest) = elem.strip_prefix("string") {
        BaseType::String {
            bound: bounded(rest)?,
        }
    } else {
        let name = match elem.split('/').collect::<Vec<_>>().as_slice() {
            [name] => TypeName::msg(package, *name),
            [pkg, name] => TypeName::msg(*pkg, *name),
            [pkg, ns, name] => TypeName {
                package: (*pkg).into(),
                namespace: (*ns).into(),
                name: (*name).into(),
            },
            _ => return Err(format!("bad type `{elem}`")),
        };
        let valid =
            |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid(&name.package) || !valid(&name.name) {
            return Err(format!("bad type `{elem}`"));
        }
        BaseType::Named(name)
    };
    Ok(FieldType { base, array })
}

/// Parses the text of a `.msg` file.
pub fn parse_message(name: TypeName, text: &str, file: &str) -> Result<MessageSpec, ParseError> {
    let mut spec = MessageSpec {
        name,
        fields: Vec::new(),
        constants: Vec::new(),
    };
    for (i, raw) in text.lines().enumerate() {
        let err = |msg: String| ParseError {
            file: file.to_owned(),
            line: i + 1,
            msg,
        };
        let line = raw.replace('\t', " ");
        let line = match line.find('#') {
            Some(c) => &line[..c],
            None => &line[..],
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (ty, rest) = line
            .split_once(' ')
            .ok_or_else(|| err(format!("expected `type name`, got `{line}`")))?;
        let rest = rest.trim_start();
        if let Some((cname, value)) = rest.split_once('=') {
            spec.constants.push(Constant {
                ty: ty.to_owned(),
                name: cname.trim().to_owned(),
                value: value.trim().to_owned(),
            });
            continue;
        }
        let (fname, default) = match rest.split_once(' ') {
            Some((n, d)) => (n, Some(d.trim().to_owned()).filter(|d| !d.is_empty())),
            None => (rest, None),
        };
        let valid_name = fname.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && fname
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !valid_name {
            return Err(err(format!("invalid field name `{fname}`")));
        }
        let ty = parse_type(ty, &spec.name.package).map_err(err)?;
        spec.fields.push(Field {
            name: fname.to_owned(),
            ty,
            default,
        });
    }
    Ok(spec)
}

/// Parses the text of a `.srv` file.
pub fn parse_service(name: TypeName, text: &str, file: &str) -> Result<ServiceSpec, ParseError> {
    let mut parts = text.splitn(2, "\n---");
    let req = parts.next().unwrap_or_default();
    let resp = match parts.next() {
        Some(r) => r.split_once('\n').map_or("", |(_, rest)| rest),
        None => {
            return Err(ParseError {
                file: file.into(),
                line: 1,
                msg: "service has no `---` separator".into(),
            });
        }
    };
    let sub = |suffix: &str| TypeName {
        name: format!("{}_{suffix}", name.name),
        ..name.clone()
    };
    Ok(ServiceSpec {
        request: parse_message(sub("Request"), req, file)?,
        response: parse_message(sub("Response"), resp, file)?,
        name,
    })
}

/// A set of parsed messages, keyed by name.
#[derive(Debug, Clone, Default)]
pub struct Interfaces {
    pub messages: BTreeMap<TypeName, MessageSpec>,
}

impl Interfaces {
    /// Adds a message, replacing one with the same name.
    pub fn add(&mut self, spec: MessageSpec) {
        self.messages.insert(spec.name.clone(), spec);
    }

    /// Loads every `msg/*.msg` and `srv/*.srv` under a package directory
    /// (whose name is the package name).
    pub fn load_package(&mut self, dir: &Path) -> anyhow::Result<()> {
        let package = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("bad package directory {}", dir.display()))?
            .to_owned();
        for (sub, ext) in [("msg", "msg"), ("srv", "srv")] {
            let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
                continue;
            };
            let mut files: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            files.sort();
            for path in files
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e == ext))
            {
                let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
                let text = std::fs::read_to_string(&path)?;
                let file = path.display().to_string();
                if ext == "msg" {
                    self.add(parse_message(TypeName::msg(&package, stem), &text, &file)?);
                } else {
                    let name = TypeName {
                        package: package.clone(),
                        namespace: "srv".into(),
                        name: stem,
                    };
                    let srv = parse_service(name, &text, &file)?;
                    self.add(srv.request);
                    self.add(srv.response);
                }
            }
        }
        Ok(())
    }

    /// Looks a message up.
    pub fn get(&self, name: &TypeName) -> Option<&MessageSpec> {
        self.messages.get(name)
    }

    /// Names of every message referenced by `name`, transitively, sorted,
    /// excluding `name` itself. Fails on a missing reference.
    pub fn references(&self, name: &TypeName) -> Result<Vec<TypeName>, TypeName> {
        let mut seen = std::collections::BTreeSet::new();
        let mut stack = vec![name.clone()];
        while let Some(n) = stack.pop() {
            let spec = self.get(&n).ok_or_else(|| n.clone())?;
            for f in &spec.fields {
                if let BaseType::Named(t) = &f.ty.base
                    && seen.insert(t.clone())
                {
                    stack.push(t.clone());
                }
            }
        }
        seen.remove(name);
        Ok(seen.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> MessageSpec {
        parse_message(TypeName::msg("pkg", "M"), text, "M.msg").unwrap()
    }

    #[test]
    fn fields_constants_comments_defaults() {
        let m = parse(
            "# file comment\n\
             uint8 MODE_A=1  # a constant\n\
             string GREETING = hello # not part of the value\n\
             \n\
             std_msgs/Header header\n\
             float64 x 0.5   # with default\n\
             float32[] ranges\n\
             int32[3] fixed\n\
             uint8[<=4] bounded\n\
             string<=10 name\n\
             string<=5[2] names\n\
             \tOther other  # same package\n\
             pkg/msg/Full full\n",
        );
        assert_eq!(m.constants.len(), 2);
        assert_eq!(m.constants[0].value, "1");
        assert_eq!(m.constants[1].value, "hello");
        let ty = |i: usize| &m.fields[i].ty;
        assert_eq!(
            ty(0).base,
            BaseType::Named(TypeName::msg("std_msgs", "Header"))
        );
        assert_eq!(m.fields[1].default.as_deref(), Some("0.5"));
        assert_eq!(ty(2).array, Array::Unbounded);
        assert_eq!(ty(3).array, Array::Fixed(3));
        assert_eq!(ty(4).array, Array::Bounded(4));
        assert_eq!(ty(5).base, BaseType::String { bound: Some(10) });
        assert_eq!(
            *ty(6),
            FieldType {
                base: BaseType::String { bound: Some(5) },
                array: Array::Fixed(2)
            }
        );
        assert_eq!(ty(7).base, BaseType::Named(TypeName::msg("pkg", "Other")));
        assert_eq!(m.fields[8].name, "full");
    }

    #[test]
    fn errors_point_at_the_line() {
        let e =
            parse_message(TypeName::msg("p", "M"), "int32 ok\nint32 Bad\n", "M.msg").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(parse_message(TypeName::msg("p", "M"), "int32[x] a", "f").is_err());
        assert!(parse_message(TypeName::msg("p", "M"), "lonely", "f").is_err());
    }

    #[test]
    fn services_split_on_separator() {
        let s = parse_service(
            TypeName {
                package: "p".into(),
                namespace: "srv".into(),
                name: "AddTwoInts".into(),
            },
            "int64 a\nint64 b\n---\nint64 sum\n",
            "AddTwoInts.srv",
        )
        .unwrap();
        assert_eq!(s.request.name.name, "AddTwoInts_Request");
        assert_eq!(s.request.fields.len(), 2);
        assert_eq!(s.response.fields[0].name, "sum");
    }

    #[test]
    fn dds_names_roundtrip() {
        let t = TypeName::msg("std_msgs", "String");
        assert_eq!(t.dds_name(), "std_msgs::msg::dds_::String_");
        assert_eq!(TypeName::from_dds(&t.dds_name()), Some(t));
        assert_eq!(TypeName::from_dds("nonsense"), None);
    }
}
