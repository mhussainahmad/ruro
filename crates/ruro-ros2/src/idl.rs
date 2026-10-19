//! Parser for ROS 2 `.msg`, `.srv` and `.action` interface definitions.
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

impl FieldType {
    /// A single value of another message type.
    pub fn named(name: TypeName) -> Self {
        Self {
            base: BaseType::Named(name),
            array: Array::None,
        }
    }

    /// A single primitive value.
    pub fn primitive(p: Primitive) -> Self {
        Self {
            base: BaseType::Primitive(p),
            array: Array::None,
        }
    }
}

/// A message name qualified by its package, e.g. `geometry_msgs/msg/Twist`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeName {
    pub package: String,
    /// `msg`, `srv` or `action`.
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

    /// `<name>_<suffix>` in the same package and namespace, e.g.
    /// `Fibonacci_Goal` for `Fibonacci`.
    pub fn suffixed(&self, suffix: &str) -> Self {
        Self {
            name: format!("{}_{suffix}", self.name),
            ..self.clone()
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

impl Field {
    /// A field without a default value.
    pub fn new(name: &str, ty: FieldType) -> Self {
        Self {
            name: name.into(),
            ty,
            default: None,
        }
    }
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

impl ServiceSpec {
    /// A service from its request and response, which must be named
    /// `<name>_Request` and `<name>_Response`.
    pub fn new(name: TypeName, request: MessageSpec, response: MessageSpec) -> Self {
        Self {
            name,
            request,
            response,
        }
    }

    /// `<Srv>_Event`, the service introspection message every ROS 2 service
    /// type carries: event info plus at most one request and one response.
    pub fn event(&self) -> MessageSpec {
        let bounded = |t: &TypeName| FieldType {
            base: BaseType::Named(t.clone()),
            array: Array::Bounded(1),
        };
        MessageSpec {
            name: self.name.suffixed("Event"),
            fields: vec![
                Field::new(
                    "info",
                    FieldType::named(TypeName::msg("service_msgs", "ServiceEventInfo")),
                ),
                Field::new("request", bounded(&self.request.name)),
                Field::new("response", bounded(&self.response.name)),
            ],
            constants: Vec::new(),
        }
    }

    /// The type description of the service itself, as `rosidl` hashes it:
    /// a message with the fields `request_message`, `response_message` and
    /// `event_message`. Its RIHS01 hash goes in rmw_zenoh key expressions.
    pub fn description(&self) -> MessageSpec {
        MessageSpec {
            name: self.name.clone(),
            fields: vec![
                Field::new(
                    "request_message",
                    FieldType::named(self.request.name.clone()),
                ),
                Field::new(
                    "response_message",
                    FieldType::named(self.response.name.clone()),
                ),
                Field::new(
                    "event_message",
                    FieldType::named(self.name.suffixed("Event")),
                ),
            ],
            constants: Vec::new(),
        }
    }

    /// Every message the service defines: request, response, event and the
    /// description.
    pub fn messages(&self) -> Vec<MessageSpec> {
        vec![
            self.request.clone(),
            self.response.clone(),
            self.event(),
            self.description(),
        ]
    }
}

/// A parsed action (`.action`): goal, result and feedback messages named
/// `<Action>_Goal`, `<Action>_Result` and `<Action>_Feedback` in the
/// `action` namespace.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionSpec {
    pub name: TypeName,
    pub goal: MessageSpec,
    pub result: MessageSpec,
    pub feedback: MessageSpec,
}

impl ActionSpec {
    fn uuid() -> FieldType {
        FieldType::named(TypeName::msg("unique_identifier_msgs", "UUID"))
    }

    /// `<Action>_SendGoal`: request `{goal_id, goal}`, response
    /// `{accepted, stamp}`.
    pub fn send_goal(&self) -> ServiceSpec {
        let name = self.name.suffixed("SendGoal");
        ServiceSpec::new(
            name.clone(),
            MessageSpec {
                name: name.suffixed("Request"),
                fields: vec![
                    Field::new("goal_id", Self::uuid()),
                    Field::new("goal", FieldType::named(self.goal.name.clone())),
                ],
                constants: Vec::new(),
            },
            MessageSpec {
                name: name.suffixed("Response"),
                fields: vec![
                    Field::new("accepted", FieldType::primitive(Primitive::Bool)),
                    Field::new(
                        "stamp",
                        FieldType::named(TypeName::msg("builtin_interfaces", "Time")),
                    ),
                ],
                constants: Vec::new(),
            },
        )
    }

    /// `<Action>_GetResult`: request `{goal_id}`, response `{status, result}`.
    pub fn get_result(&self) -> ServiceSpec {
        let name = self.name.suffixed("GetResult");
        ServiceSpec::new(
            name.clone(),
            MessageSpec {
                name: name.suffixed("Request"),
                fields: vec![Field::new("goal_id", Self::uuid())],
                constants: Vec::new(),
            },
            MessageSpec {
                name: name.suffixed("Response"),
                fields: vec![
                    Field::new("status", FieldType::primitive(Primitive::Int8)),
                    Field::new("result", FieldType::named(self.result.name.clone())),
                ],
                constants: Vec::new(),
            },
        )
    }

    /// `<Action>_FeedbackMessage`: `{goal_id, feedback}`, published on the
    /// feedback topic.
    pub fn feedback_message(&self) -> MessageSpec {
        MessageSpec {
            name: self.name.suffixed("FeedbackMessage"),
            fields: vec![
                Field::new("goal_id", Self::uuid()),
                Field::new("feedback", FieldType::named(self.feedback.name.clone())),
            ],
            constants: Vec::new(),
        }
    }

    /// The type description of the action itself, as `rosidl` hashes it.
    pub fn description(&self) -> MessageSpec {
        let nested = |suffix: &str| FieldType::named(self.name.suffixed(suffix));
        MessageSpec {
            name: self.name.clone(),
            fields: vec![
                Field::new("goal", nested("Goal")),
                Field::new("result", nested("Result")),
                Field::new("feedback", nested("Feedback")),
                Field::new("send_goal_service", nested("SendGoal")),
                Field::new("get_result_service", nested("GetResult")),
                Field::new("feedback_message", nested("FeedbackMessage")),
            ],
            constants: Vec::new(),
        }
    }

    /// Every message the action defines, its two services' included.
    pub fn messages(&self) -> Vec<MessageSpec> {
        let mut m = vec![
            self.goal.clone(),
            self.result.clone(),
            self.feedback.clone(),
            self.feedback_message(),
            self.description(),
        ];
        m.extend(self.send_goal().messages());
        m.extend(self.get_result().messages());
        m
    }
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

/// Splits a `.srv` or `.action` text at its `---` lines into sections, each
/// with the number of lines before it.
fn sections(text: &str) -> Vec<(usize, String)> {
    let mut out = vec![(0, String::new())];
    for (i, line) in text.lines().enumerate() {
        if line.trim() == "---" {
            out.push((i + 1, String::new()));
        } else {
            let cur = &mut out.last_mut().expect("one section").1;
            cur.push_str(line);
            cur.push('\n');
        }
    }
    out
}

/// Parses one section, reporting errors at their line in the whole file.
fn parse_section(
    name: TypeName,
    (offset, text): &(usize, String),
    file: &str,
) -> Result<MessageSpec, ParseError> {
    parse_message(name, text, file).map_err(|mut e| {
        e.line += offset;
        e
    })
}

/// Parses the text of a `.srv` file.
pub fn parse_service(name: TypeName, text: &str, file: &str) -> Result<ServiceSpec, ParseError> {
    let parts = sections(text);
    if parts.len() != 2 {
        return Err(ParseError {
            file: file.into(),
            line: 1,
            msg: format!(
                "a service needs exactly one `---` separator, found {}",
                parts.len() - 1
            ),
        });
    }
    Ok(ServiceSpec {
        request: parse_section(name.suffixed("Request"), &parts[0], file)?,
        response: parse_section(name.suffixed("Response"), &parts[1], file)?,
        name,
    })
}

/// Parses the text of a `.action` file.
pub fn parse_action(name: TypeName, text: &str, file: &str) -> Result<ActionSpec, ParseError> {
    let parts = sections(text);
    if parts.len() != 3 {
        return Err(ParseError {
            file: file.into(),
            line: 1,
            msg: format!(
                "an action needs exactly two `---` separators, found {}",
                parts.len() - 1
            ),
        });
    }
    Ok(ActionSpec {
        goal: parse_section(name.suffixed("Goal"), &parts[0], file)?,
        result: parse_section(name.suffixed("Result"), &parts[1], file)?,
        feedback: parse_section(name.suffixed("Feedback"), &parts[2], file)?,
        name,
    })
}

/// A set of parsed interfaces, keyed by name.
///
/// `messages` holds every message type, including the request, response
/// and event messages of services and the messages actions are made of, as
/// well as the type descriptions of services and actions themselves (so
/// [`type_hash`](crate::hash::type_hash) works for `pkg/srv/X` and
/// `pkg/action/X`). `services` and `actions` record which names are
/// services and actions; the `SendGoal` and `GetResult` services of an
/// action are in `services` too.
#[derive(Debug, Clone, Default)]
pub struct Interfaces {
    pub messages: BTreeMap<TypeName, MessageSpec>,
    pub services: BTreeMap<TypeName, ServiceSpec>,
    pub actions: BTreeMap<TypeName, ActionSpec>,
}

impl Interfaces {
    /// Adds a message, replacing one with the same name.
    pub fn add(&mut self, spec: MessageSpec) {
        self.messages.insert(spec.name.clone(), spec);
    }

    /// Adds a service and its messages.
    pub fn add_service(&mut self, spec: ServiceSpec) {
        for m in spec.messages() {
            self.add(m);
        }
        self.services.insert(spec.name.clone(), spec);
    }

    /// Adds an action, its messages and its services.
    pub fn add_action(&mut self, spec: ActionSpec) {
        for m in spec.messages() {
            self.add(m);
        }
        for s in [spec.send_goal(), spec.get_result()] {
            self.services.insert(s.name.clone(), s);
        }
        self.actions.insert(spec.name.clone(), spec);
    }

    /// Whether `name` is the type description of a service or an action
    /// rather than a message that goes on the wire.
    pub fn is_description(&self, name: &TypeName) -> bool {
        self.services.contains_key(name) || self.actions.contains_key(name)
    }

    /// Removes everything defined by `package`.
    pub fn remove_package(&mut self, package: &str) {
        self.messages.retain(|t, _| t.package != package);
        self.services.retain(|t, _| t.package != package);
        self.actions.retain(|t, _| t.package != package);
    }

    /// Parses and adds the text of a `.msg`, `.srv` or `.action` file of
    /// kind `msg`, `srv` or `action`.
    pub fn add_text(
        &mut self,
        package: &str,
        kind: &str,
        name: &str,
        text: &str,
        file: &str,
    ) -> Result<(), ParseError> {
        let ty = TypeName {
            package: package.into(),
            namespace: kind.into(),
            name: name.into(),
        };
        match kind {
            "msg" => self.add(parse_message(ty, text, file)?),
            "srv" => self.add_service(parse_service(ty, text, file)?),
            "action" => self.add_action(parse_action(ty, text, file)?),
            other => {
                return Err(ParseError {
                    file: file.into(),
                    line: 0,
                    msg: format!("unknown interface kind `{other}`"),
                });
            }
        }
        Ok(())
    }

    /// Loads every `msg/*.msg`, `srv/*.srv` and `action/*.action` under a
    /// package directory (whose name is the package name).
    pub fn load_package(&mut self, dir: &Path) -> anyhow::Result<()> {
        let package = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("bad package directory {}", dir.display()))?
            .to_owned();
        for kind in ["msg", "srv", "action"] {
            let Ok(entries) = std::fs::read_dir(dir.join(kind)) else {
                continue;
            };
            let mut files: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            files.sort();
            for path in files
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e == kind))
            {
                let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
                let text = std::fs::read_to_string(&path)?;
                let file = path.display().to_string();
                self.add_text(&package, kind, &stem, &text, &file)?;
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

        // std_srvs/Trigger: an empty request, the separator on line 1.
        let name = |n: &str| TypeName {
            package: "std_srvs".into(),
            namespace: "srv".into(),
            name: n.into(),
        };
        let t = parse_service(
            name("Trigger"),
            "---\nbool success # ok\nstring message\n",
            "Trigger.srv",
        )
        .unwrap();
        assert!(t.request.fields.is_empty());
        assert_eq!(t.response.fields.len(), 2);
        let e = parse_service(name("Empty"), "---\n", "Empty.srv").unwrap();
        assert!(e.request.fields.is_empty() && e.response.fields.is_empty());
        assert!(parse_service(name("X"), "int32 a\n", "X.srv").is_err());
        let err = parse_service(name("X"), "int32 a\n---\nint32 Bad\n", "X.srv").unwrap_err();
        assert_eq!(err.line, 3, "lines count from the top of the file");

        let event = t.event();
        assert_eq!(event.name.to_string(), "std_srvs/srv/Trigger_Event");
        assert_eq!(event.fields[1].ty.array, Array::Bounded(1));
        let d = t.description();
        assert_eq!(d.name.to_string(), "std_srvs/srv/Trigger");
        assert_eq!(d.fields[2].name, "event_message");
    }

    #[test]
    fn actions_have_goal_result_feedback_and_services() {
        let name = TypeName {
            package: "p".into(),
            namespace: "action".into(),
            name: "Fibonacci".into(),
        };
        let a = parse_action(
            name.clone(),
            "int32 order\n---\nint32[] sequence\n---\nint32[] partial_sequence\n",
            "Fibonacci.action",
        )
        .unwrap();
        assert_eq!(a.goal.name.to_string(), "p/action/Fibonacci_Goal");
        assert_eq!(a.feedback.fields[0].name, "partial_sequence");
        assert!(parse_action(name.clone(), "int32 a\n---\n", "f").is_err());

        let mut i = Interfaces::default();
        i.add_action(a);
        for t in [
            "Fibonacci",
            "Fibonacci_Goal",
            "Fibonacci_Result",
            "Fibonacci_Feedback",
            "Fibonacci_FeedbackMessage",
            "Fibonacci_SendGoal",
            "Fibonacci_SendGoal_Request",
            "Fibonacci_SendGoal_Response",
            "Fibonacci_SendGoal_Event",
            "Fibonacci_GetResult",
            "Fibonacci_GetResult_Request",
            "Fibonacci_GetResult_Response",
            "Fibonacci_GetResult_Event",
        ] {
            let t = TypeName {
                name: t.into(),
                ..name.clone()
            };
            assert!(i.get(&t).is_some(), "{t}");
        }
        assert_eq!(i.messages.len(), 13);
        assert!(i.is_description(&name));
        assert!(i.is_description(&name.suffixed("SendGoal")));
        assert!(!i.is_description(&name.suffixed("Goal")));
        i.remove_package("p");
        assert!(i.messages.is_empty() && i.services.is_empty() && i.actions.is_empty());
    }

    #[test]
    fn dds_names_roundtrip() {
        let t = TypeName::msg("std_msgs", "String");
        assert_eq!(t.dds_name(), "std_msgs::msg::dds_::String_");
        assert_eq!(TypeName::from_dds(&t.dds_name()), Some(t));
        assert_eq!(TypeName::from_dds("nonsense"), None);
    }
}
