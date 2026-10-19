//! Parser for `.idl` interface files, the OMG IDL subset that `rosidl`
//! generates from `.msg` files and that packages may write directly.
//!
//! rosbag2 stores some type definitions in this form (schema encoding
//! `ros2idl`), and `share/<package>/msg/` holds a `.idl` next to every
//! `.msg`. Supported: nested `module`s, `struct`s, `const`s in a
//! `<Type>_Constants` module, `typedef`s of arrays, `sequence<T>` and
//! `sequence<T, N>`, bounded strings, fixed arrays and `@default`
//! annotations; other annotations, comments and `#include` lines are
//! skipped.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};

use crate::idl::{Array, BaseType, Constant, Field, FieldType, MessageSpec, Primitive, TypeName};

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Punct(char),
}

fn tokenize(text: &str) -> Result<Vec<Tok>> {
    let mut toks = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut line_start = true;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Preprocessor lines (`#include`).
        if c == '#' && line_start {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        line_start = false;
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == '"' {
            let mut s = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    None => bail!("unterminated string"),
                    Some('"') => break,
                    Some('\\') => {
                        match chars.get(i + 1) {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(&e) => s.push(e),
                            None => bail!("unterminated string"),
                        }
                        i += 2;
                        continue;
                    }
                    Some(&ch) => s.push(ch),
                }
                i += 1;
            }
            i += 1;
            // Adjacent literals concatenate.
            if let Some(Tok::Str(prev)) = toks.last_mut() {
                prev.push_str(&s);
            } else {
                toks.push(Tok::Str(s));
            }
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || chars[i] == '_'
                    || (chars[i] == ':' && chars.get(i + 1) == Some(&':'))
                    || (chars[i] == ':' && i > 0 && chars[i - 1] == ':'))
            {
                i += 1;
            }
            toks.push(Tok::Ident(chars[start..i].iter().collect()));
            continue;
        }
        if c.is_ascii_digit()
            || ((c == '-' || c == '+' || c == '.')
                && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || chars[i] == '.'
                    || ((chars[i] == '-' || chars[i] == '+') && matches!(chars[i - 1], 'e' | 'E')))
            {
                i += 1;
            }
            toks.push(Tok::Num(chars[start..i].iter().collect()));
            continue;
        }
        toks.push(Tok::Punct(c));
        i += 1;
    }
    Ok(toks)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    /// Typedefs: aliases and fixed arrays.
    typedefs: BTreeMap<String, FieldType>,
    /// Constants per struct, from `<Type>_Constants` modules.
    constants: BTreeMap<TypeName, Vec<Constant>>,
    specs: Vec<MessageSpec>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Result<Tok> {
        let t = self
            .toks
            .get(self.pos)
            .cloned()
            .ok_or_else(|| anyhow!("unexpected end of IDL"))?;
        self.pos += 1;
        Ok(t)
    }

    fn punct(&mut self, c: char) -> Result<()> {
        match self.next()? {
            Tok::Punct(p) if p == c => Ok(()),
            t => bail!("expected `{c}`, got {t:?}"),
        }
    }

    fn eat_punct(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Punct(c)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Result<String> {
        match self.next()? {
            Tok::Ident(s) => Ok(s),
            t => bail!("expected a name, got {t:?}"),
        }
    }

    fn number(&mut self) -> Result<usize> {
        match self.next()? {
            Tok::Num(n) => n.parse().map_err(|_| anyhow!("bad size `{n}`")),
            t => bail!("expected a number, got {t:?}"),
        }
    }

    /// Annotations before a declaration; returns the `@default` value.
    fn annotations(&mut self) -> Result<Option<String>> {
        let mut default = None;
        while self.eat_punct('@') {
            let name = self.ident()?;
            if !self.eat_punct('(') {
                continue;
            }
            let mut depth = 1;
            let mut args: Vec<Tok> = Vec::new();
            while depth > 0 {
                let t = self.next()?;
                match t {
                    Tok::Punct('(') => depth += 1,
                    Tok::Punct(')') => depth -= 1,
                    _ => {}
                }
                if depth > 0 {
                    args.push(t);
                }
            }
            if name == "default"
                && let [Tok::Ident(k), Tok::Punct('='), rest @ ..] = args.as_slice()
                && k == "value"
            {
                default = Some(literal(rest));
            }
        }
        Ok(default)
    }

    fn module(&mut self, path: &mut Vec<String>) -> Result<()> {
        let name = self.ident()?;
        self.punct('{')?;
        path.push(name);
        while !self.eat_punct('}') {
            self.definition(path)?;
        }
        path.pop();
        self.eat_punct(';');
        Ok(())
    }

    fn definition(&mut self, path: &mut Vec<String>) -> Result<()> {
        self.annotations()?;
        let kw = self.ident()?;
        match kw.as_str() {
            "module" => self.module(path),
            "struct" => self.structure(path),
            "typedef" => {
                let mut ty = self.member_type(path)?;
                let name = self.ident()?;
                if self.eat_punct('[') {
                    if ty.array != Array::None {
                        bail!("typedef {name}: arrays of arrays are not supported");
                    }
                    ty.array = Array::Fixed(self.number()?);
                    self.punct(']')?;
                }
                self.punct(';')?;
                self.typedefs.insert(name, ty);
                Ok(())
            }
            "const" => {
                let ty = self.const_type()?;
                let name = self.ident()?;
                self.punct('=')?;
                let mut value = Vec::new();
                while !self.eat_punct(';') {
                    value.push(self.next()?);
                }
                let owner = path
                    .last()
                    .and_then(|m| m.strip_suffix("_Constants"))
                    .ok_or_else(|| anyhow!("constant {name} outside a _Constants module"))?;
                let t = type_in(path, owner)?;
                self.constants.entry(t).or_default().push(Constant {
                    ty,
                    name,
                    value: literal(&value),
                });
                Ok(())
            }
            other => bail!("unsupported IDL declaration `{other}`"),
        }
    }

    fn const_type(&mut self) -> Result<String> {
        let t = self.base_type(&[])?;
        Ok(match t {
            BaseType::Primitive(p) => p.name().to_owned(),
            BaseType::String { .. } => "string".into(),
            BaseType::WString { .. } => "wstring".into(),
            BaseType::Named(n) => bail!("constant of type {n}"),
        })
    }

    fn base_type(&mut self, path: &[String]) -> Result<BaseType> {
        let word = self.ident()?;
        let prim = |p| Ok(BaseType::Primitive(p));
        match word.as_str() {
            "boolean" => prim(Primitive::Bool),
            "octet" => prim(Primitive::Byte),
            "char" => prim(Primitive::Char),
            "int8" => prim(Primitive::Int8),
            "uint8" => prim(Primitive::Uint8),
            "int16" | "short" => prim(Primitive::Int16),
            "uint16" => prim(Primitive::Uint16),
            "int32" => prim(Primitive::Int32),
            "uint32" => prim(Primitive::Uint32),
            "int64" => prim(Primitive::Int64),
            "uint64" => prim(Primitive::Uint64),
            "float" => prim(Primitive::Float32),
            "double" => prim(Primitive::Float64),
            "long" => {
                if self.peek() == Some(&Tok::Ident("long".into())) {
                    self.pos += 1;
                    prim(Primitive::Int64)
                } else if self.peek() == Some(&Tok::Ident("double".into())) {
                    bail!("`long double` is not supported")
                } else {
                    prim(Primitive::Int32)
                }
            }
            "unsigned" => match self.ident()?.as_str() {
                "short" => prim(Primitive::Uint16),
                "long" => {
                    if self.peek() == Some(&Tok::Ident("long".into())) {
                        self.pos += 1;
                        prim(Primitive::Uint64)
                    } else {
                        prim(Primitive::Uint32)
                    }
                }
                other => bail!("unsupported type `unsigned {other}`"),
            },
            "string" | "wstring" => {
                let bound = if self.eat_punct('<') {
                    let n = self.number()?;
                    self.punct('>')?;
                    Some(n)
                } else {
                    None
                };
                Ok(if word == "string" {
                    BaseType::String { bound }
                } else {
                    BaseType::WString { bound }
                })
            }
            "wchar" | "any" | "fixed" => bail!("IDL type `{word}` is not supported"),
            alias
                if self
                    .typedefs
                    .get(alias)
                    .is_some_and(|t| t.array == Array::None) =>
            {
                Ok(self.typedefs[alias].base.clone())
            }
            scoped => {
                let parts: Vec<&str> = scoped.split("::").collect();
                Ok(BaseType::Named(match parts.as_slice() {
                    [pkg, ns, name] => TypeName {
                        package: (*pkg).into(),
                        namespace: (*ns).into(),
                        name: (*name).into(),
                    },
                    [name] => type_in(path, name)?,
                    _ => bail!("unsupported type name `{scoped}`"),
                }))
            }
        }
    }

    /// A member type: base type, `sequence<..>` or a typedef'd array.
    fn member_type(&mut self, path: &[String]) -> Result<FieldType> {
        if self.peek() == Some(&Tok::Ident("sequence".into())) {
            self.pos += 1;
            self.punct('<')?;
            let base = self.base_type(path)?;
            let array = if self.eat_punct(',') {
                Array::Bounded(self.number()?)
            } else {
                Array::Unbounded
            };
            self.punct('>')?;
            return Ok(FieldType { base, array });
        }
        if let Some(Tok::Ident(name)) = self.peek()
            && let Some(ty) = self.typedefs.get(name).cloned()
        {
            self.pos += 1;
            return Ok(ty);
        }
        Ok(FieldType {
            base: self.base_type(path)?,
            array: Array::None,
        })
    }

    fn structure(&mut self, path: &[String]) -> Result<()> {
        let name = self.ident()?;
        let ty = type_in(path, &name)?;
        self.punct('{')?;
        let mut fields = Vec::new();
        while !self.eat_punct('}') {
            let default = self.annotations()?;
            if self.eat_punct('}') {
                break;
            }
            let mut fty = self.member_type(path)?;
            let fname = self.ident()?;
            if self.eat_punct('[') {
                let n = self.number()?;
                self.punct(']')?;
                if fty.array != Array::None {
                    bail!("{ty}.{fname}: arrays of sequences are not supported");
                }
                fty.array = Array::Fixed(n);
            }
            self.punct(';')?;
            fields.push(Field {
                name: fname,
                ty: fty,
                default,
            });
        }
        self.eat_punct(';');
        // rosidl gives empty messages a placeholder member.
        if fields.len() == 1 && fields[0].name == "structure_needs_at_least_one_member" {
            fields.clear();
        }
        self.specs.push(MessageSpec {
            name: ty,
            fields,
            constants: Vec::new(),
        });
        Ok(())
    }
}

/// The type `name` declared inside modules `path` (`[package, namespace]`).
fn type_in(path: &[String], name: &str) -> Result<TypeName> {
    match path {
        [pkg, ns, ..] => Ok(TypeName {
            package: pkg.clone(),
            namespace: ns.clone(),
            name: name.to_owned(),
        }),
        _ => {
            bail!("{name} is not inside `module <package> {{ module <msg|srv|action> {{ .. }} }}`")
        }
    }
}

/// An IDL literal in `.msg` syntax: `TRUE` becomes `true`, a string stays
/// quoted, a tuple `"(1, 2)"` becomes `[1, 2]`.
fn literal(toks: &[Tok]) -> String {
    let mut out = String::new();
    for t in toks {
        match t {
            Tok::Ident(s) if s == "TRUE" => out.push_str("true"),
            Tok::Ident(s) if s == "FALSE" => out.push_str("false"),
            Tok::Ident(s) | Tok::Num(s) => out.push_str(s),
            Tok::Punct(c) => out.push(*c),
            Tok::Str(s) => {
                let t = s.trim();
                if t.starts_with('(') && t.ends_with(')') {
                    out.push('[');
                    out.push_str(&t[1..t.len() - 1]);
                    out.push(']');
                } else {
                    out.push('"');
                    out.push_str(&s.replace('\\', "\\\\").replace('"', "\\\""));
                    out.push('"');
                }
            }
        }
    }
    out
}

/// Parses the text of an `.idl` file into the messages it declares.
pub fn parse_idl(text: &str) -> Result<Vec<MessageSpec>> {
    let mut p = Parser {
        toks: tokenize(text)?,
        pos: 0,
        typedefs: BTreeMap::new(),
        constants: BTreeMap::new(),
        specs: Vec::new(),
    };
    let mut path = Vec::new();
    while p.peek().is_some() {
        p.definition(&mut path)?;
    }
    let mut specs = p.specs;
    for s in &mut specs {
        if let Some(c) = p.constants.remove(&s.name) {
            s.constants = c;
        }
    }
    Ok(specs)
}

/// Parses a `ros2idl` schema as rosbag2 writes it: IDL files, each after a
/// line of `=` and a line `IDL: pkg/msg/Type`.
pub fn parse_concatenated_idl(text: &str) -> Result<Vec<MessageSpec>> {
    let mut specs = Vec::new();
    let mut section = String::new();
    let flush = |section: &str, specs: &mut Vec<MessageSpec>| -> Result<()> {
        if !section.trim().is_empty() {
            specs.extend(parse_idl(section)?);
        }
        Ok(())
    };
    for line in text.lines() {
        if line.trim_end().len() >= 3 && line.trim_end().chars().all(|c| c == '=') {
            flush(&section, &mut specs)?;
            section.clear();
        } else if line.starts_with("IDL: ") {
            continue;
        } else {
            section.push_str(line);
            section.push('\n');
        }
    }
    flush(&section, &mut specs)?;
    Ok(specs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAV_SAT_STATUS: &str = r#"
// generated from rosidl_adapter/resource/msg.idl.em
#include "std_msgs/msg/Header.idl"
module sensor_msgs {
  module msg {
    module NavSatStatus_Constants {
      @verbatim (language="comment", text=
        "status is not yet set" "\n" "more")
      const int8 STATUS_UNKNOWN = -2;
      const uint16 SERVICE_GPS = 1;
      const string NAME = "a \"b\"";
    };
    typedef double double__9[9];
    @verbatim (language="comment", text="A struct.")
    struct NavSatStatus {
      @default (value=-2)
      int8 status;
      uint16 service;
      double__9 covariance;
      sequence<std_msgs::msg::Header, 1> headers;
      sequence<float> values;
      string<5> short_name;
      @default (value=TRUE)
      boolean flag;
      @default (value="(1.0, 2.0)")
      double pair[2];
      unsigned long long big;
    };
  };
};
"#;

    #[test]
    fn rosidl_generated_idl_parses() {
        let specs = parse_idl(NAV_SAT_STATUS).unwrap();
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.name, TypeName::msg("sensor_msgs", "NavSatStatus"));
        assert_eq!(s.constants.len(), 3);
        assert_eq!(s.constants[0].value, "-2");
        assert_eq!(s.constants[2].value, "\"a \\\"b\\\"\"");
        let f = |n: &str| s.fields.iter().find(|f| f.name == n).unwrap();
        assert_eq!(f("status").default.as_deref(), Some("-2"));
        assert_eq!(f("covariance").ty.array, Array::Fixed(9));
        assert_eq!(f("headers").ty.array, Array::Bounded(1));
        assert_eq!(
            f("headers").ty.base,
            BaseType::Named(TypeName::msg("std_msgs", "Header"))
        );
        assert_eq!(f("values").ty.array, Array::Unbounded);
        assert_eq!(f("short_name").ty.base, BaseType::String { bound: Some(5) });
        assert_eq!(f("flag").default.as_deref(), Some("true"));
        assert_eq!(f("pair").default.as_deref(), Some("[1.0, 2.0]"));
        assert_eq!(f("big").ty.base, BaseType::Primitive(Primitive::Uint64));
    }

    #[test]
    fn concatenated_schemas_and_empty_messages() {
        let text = "================================================================================\n\
                    IDL: p/msg/E\n\
                    module p { module msg { struct E { uint8 structure_needs_at_least_one_member; }; }; };\n\
                    ================================================================================\n\
                    IDL: p/msg/F\n\
                    module p { module msg { struct F { p::msg::E e; }; }; };\n";
        let specs = parse_concatenated_idl(text).unwrap();
        assert_eq!(specs.len(), 2);
        assert!(specs[0].fields.is_empty());
        assert!(parse_idl("module p { struct X { int32 a; }; };").is_err());
        assert!(parse_idl("module p { module msg { struct X { wchar a; }; }; };").is_err());
    }
}
