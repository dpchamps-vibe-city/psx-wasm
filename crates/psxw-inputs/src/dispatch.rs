//! `[dispatch]`: explicit binding facts (plans/dispatch-binding). Which
//! containers are executable placements, which software operations write
//! code, and what each operation publishes or invalidates. Facts describe
//! original operations; they never replace them.
//!
//! [`DispatchSection::check`] validates the complete schema without the
//! address map: shapes, IDs, value sources and event availability. The
//! build resolves names and decides what is implemented.

use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// The dispatch input schema this build reads.
pub const SCHEMA_VERSION: u32 = 1;

/// Most o32 argument words a value source may name.
pub const MAX_ARG: u32 = 32;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchSection {
    pub schema_version: u32,
    #[serde(default)]
    pub require_explicit: bool,
    #[serde(default)]
    pub placements: Vec<Placement>,
    #[serde(default)]
    pub writers: Vec<Writer>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub c_continuations: Vec<CContinuation>,
}

/// A container's compiled original layout: its segments, text and data
/// come from the container's own metadata.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub id: String,
    pub container: String,
}

/// Possible writes of one operation: half-open guest address ranges.
/// The build infers bounds first; declared `destinations` supply what
/// analysis can't establish, and are checked against the writes it knows.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Writer {
    pub id: String,
    pub container: Option<String>,
    pub function: Option<String>,
    pub pc: Option<u32>,
    pub boundary: Option<String>,
    #[serde(default)]
    pub destinations: Vec<Span>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub container: Option<String>,
    pub function: Option<String>,
    pub pc: Option<u32>,
    pub boundary: Option<String>,
    pub event: Event,
    pub c_marker: Option<String>,
    pub effect: Effect,
    pub placement: Option<String>,
    pub selector: Option<String>,
    #[serde(default)]
    pub cases: BTreeMap<String, String>,
    pub destination: Option<String>,
    pub length: Option<String>,
    pub source_offset: Option<String>,
    pub success: Option<Success>,
    pub request: Option<String>,
    pub request_group: Option<String>,
}

/// A source-owned C continuation registration (milestone C's format).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CContinuation {
    pub slot: String,
    pub placement: String,
    pub anchor: u32,
    pub function: String,
    pub state_header: String,
    pub state_type: String,
    pub bridge: Option<String>,
    #[serde(default)]
    pub bridge_inputs: Vec<String>,
    #[serde(default)]
    pub bridge_outputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Event {
    Enter,
    Return,
    AfterWrite,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    Begin,
    Invalidate,
    Publish,
    End,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Success {
    #[default]
    Always,
    Zero,
    Nonzero,
}

/// One operation site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Site {
    /// A qualified function.
    Function { container: String, function: String },
    /// An original instruction address.
    Pc { container: String, pc: u32 },
    /// A registered native software boundary.
    Boundary(String),
}

impl std::fmt::Display for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Site::Function { container, function } => write!(f, "{container}:{function}"),
            Site::Pc { container, pc } => write!(f, "{container}@0x{pc:08X}"),
            Site::Boundary(b) => write!(f, "boundary {b}"),
        }
    }
}

/// A scalar value source: a u32 at the rule's event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Const(u32),
    /// Original o32 argument word, captured at entry.
    Arg(u32),
    /// The first o32 return word.
    Return,
    /// One u32 at a symbol, read at the event.
    Global { container: String, symbol: String },
    /// One u32 at `offset` bytes through argument `arg`, read at the event.
    Field { arg: u32, offset: u32 },
    WriteAddress,
    WriteSize,
}

/// Where a publication's bytes come from in the container's source image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offset {
    Value(Value),
    /// From the destination and the placement's segment mapping.
    Placement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Literal(String),
    /// An exhaustive value-to-placement mapping, in value order.
    Selector { value: Value, cases: Vec<(u32, String)> },
    /// The placement the request's begin-context captured.
    Request,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The innermost live begin-context of this call.
    Call,
    /// A request shared across asynchronous boundaries.
    Keyed { key: Value, group: String },
}

/// A rule after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleFacts {
    pub id: String,
    pub site: Site,
    pub event: Event,
    pub c_marker: Option<String>,
    pub effect: Effect,
    pub target: Option<Target>,
    pub destination: Option<Value>,
    pub length: Option<Value>,
    pub source_offset: Option<Offset>,
    pub success: Success,
    pub request: Option<Request>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterFacts {
    pub id: String,
    pub site: Site,
    pub destinations: Vec<Span>,
}

/// The section after validation.
#[derive(Debug, Clone)]
pub struct Facts {
    pub require_explicit: bool,
    pub placements: Vec<Placement>,
    pub writers: Vec<WriterFacts>,
    pub rules: Vec<RuleFacts>,
    pub c_continuations: Vec<CContinuation>,
}

/// The header authored C includes for its explicit write sites.
pub const HEADER: &str = "psxw_binding.h";
/// A `c_marker` site's function: this prefix, then the marker.
pub const SITE_PREFIX: &str = "psxw_binding_site_";

/// `psxw_binding.h`: one declaration per `c_marker`. The authored C calls
/// it immediately after the write its rule describes, with the event's
/// normalized values (not the enclosing function's arguments).
pub fn header(section: &DispatchSection) -> String {
    let mut s = String::from(
        "/* Generated by psx-wasm from [dispatch] (plans/dispatch-binding). */\n\
         #ifndef PSXW_BINDING_H\n#define PSXW_BINDING_H\n\
         typedef unsigned int psxw_binding_u32;\n",
    );
    for r in section.rules.iter().filter(|r| r.c_marker.is_some()) {
        let m = r.c_marker.as_deref().unwrap_or_default();
        s += &format!(
            "/* rule `{}` */\nvoid {SITE_PREFIX}{m}(psxw_binding_u32 destination, psxw_binding_u32 length, \
             psxw_binding_u32 source_offset, psxw_binding_u32 selector, psxw_binding_u32 request_key);\n",
            r.id
        );
    }
    s + "#endif\n"
}

/// A document holding only `[dispatch]` (fixtures, tests).
pub fn parse(text: &str) -> Result<DispatchSection> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Doc {
        dispatch: DispatchSection,
    }
    Ok(toml::from_str::<Doc>(text).context("binding.invalid_fact: [dispatch]")?.dispatch)
}

fn ident(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_') && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn site(what: &str, container: &Option<String>, function: &Option<String>, pc: Option<u32>, boundary: &Option<String>) -> Result<Site> {
    Ok(match (container, function, pc, boundary) {
        (Some(c), Some(f), None, None) => Site::Function { container: c.clone(), function: f.clone() },
        (Some(c), None, Some(pc), None) => {
            ensure!(pc % 4 == 0, "{what}: pc 0x{pc:08X} is not an instruction address");
            Site::Pc { container: c.clone(), pc }
        }
        (None, None, None, Some(b)) => Site::Boundary(b.clone()),
        _ => bail!("{what}: name exactly one site: `container` + `function`, `container` + `pc`, or `boundary`"),
    })
}

/// Parse a scalar value source.
pub fn value(text: &str) -> Result<Value> {
    let num = |s: &str| crate::parse_u32(s).with_context(|| format!("`{text}`: `{s}` is not a u32"));
    let arg = |s: &str| -> Result<u32> {
        let a = num(s)?;
        ensure!(a < MAX_ARG, "`{text}`: argument word {a} is past the last ({})", MAX_ARG - 1);
        Ok(a)
    };
    let mut parts = text.splitn(3, ':');
    Ok(match (parts.next(), parts.next(), parts.next()) {
        (Some("const"), Some(v), None) => Value::Const(num(v)?),
        (Some("arg"), Some(i), None) => Value::Arg(arg(i)?),
        (Some("return"), None, None) => Value::Return,
        (Some("global"), Some(c), Some(s)) if !c.is_empty() && !s.is_empty() => {
            Value::Global { container: c.to_string(), symbol: s.to_string() }
        }
        (Some("field"), Some(i), Some(o)) => Value::Field { arg: arg(i)?, offset: num(o)? },
        (Some("write-address"), None, None) => Value::WriteAddress,
        (Some("write-size"), None, None) => Value::WriteSize,
        _ => bail!(
            "`{text}` is not a value source (const:<u32>, arg:<index>, return, global:<container>:<symbol>, \
             field:<arg>:<offset>, write-address, write-size)"
        ),
    })
}

impl DispatchSection {
    /// Validate shapes, IDs, value sources and event availability.
    pub fn check(&self) -> Result<Facts> {
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "binding.invalid_fact: [dispatch] schema_version {} (this build reads {SCHEMA_VERSION})",
            self.schema_version
        );
        let unique = |what: &str, ids: &mut dyn Iterator<Item = &String>| -> Result<()> {
            let mut seen = BTreeSet::new();
            for id in ids {
                ensure!(!id.is_empty(), "binding.invalid_fact: a {what} has an empty id");
                ensure!(seen.insert(id), "binding.invalid_fact: two {what}s have id `{id}`");
            }
            Ok(())
        };
        unique("placement", &mut self.placements.iter().map(|p| &p.id))?;
        unique("placement container", &mut self.placements.iter().map(|p| &p.container))?;
        unique("writer", &mut self.writers.iter().map(|w| &w.id))?;
        unique("rule", &mut self.rules.iter().map(|r| &r.id))?;
        let placements: BTreeSet<&str> = self.placements.iter().map(|p| p.id.as_str()).collect();
        let writers = self
            .writers
            .iter()
            .map(|w| {
                let what = format!("binding.invalid_fact: writer `{}`", w.id);
                let site = site(&what, &w.container, &w.function, w.pc, &w.boundary)?;
                for d in &w.destinations {
                    ensure!(d.start < d.end, "{what}: destination 0x{:08X}..0x{:08X} is empty", d.start, d.end);
                }
                Ok(WriterFacts { id: w.id.clone(), site, destinations: w.destinations.clone() })
            })
            .collect::<Result<_>>()?;
        let rules = self.rules.iter().map(|r| r.check(&placements)).collect::<Result<_>>()?;
        for c in &self.c_continuations {
            let what = format!("binding.invalid_fact: c_continuation `{}`", c.slot);
            ensure!(ident(&c.slot), "{what}: `slot` is not a C identifier");
            ensure!(placements.contains(c.placement.as_str()), "{what}: no placement `{}`", c.placement);
            ensure!(c.bridge.is_some() || c.bridge_inputs.is_empty() && c.bridge_outputs.is_empty(), "{what}: bridge values without a `bridge`");
        }
        Ok(Facts {
            require_explicit: self.require_explicit,
            placements: self.placements.clone(),
            writers,
            rules,
            c_continuations: self.c_continuations.clone(),
        })
    }
}

impl Rule {
    fn check(&self, placements: &BTreeSet<&str>) -> Result<RuleFacts> {
        let what = format!("binding.invalid_fact: rule `{}`", self.id);
        let site = site(&what, &self.container, &self.function, self.pc, &self.boundary)?;
        let src = |field: &str, v: &Option<String>| -> Result<Option<Value>> {
            v.as_deref().map(value).transpose().with_context(|| format!("{what}: field `{field}`"))
        };
        let need = |field: &str, v: Option<Value>| v.with_context(|| format!("{what}: field `{field}` is required for effect {:?}", self.effect));
        let absent = |field: &str, present: bool| -> Result<()> {
            ensure!(!present, "{what}: field `{field}` does not apply to effect {:?}", self.effect);
            Ok(())
        };
        if let Some(m) = &self.c_marker {
            ensure!(ident(m), "{what}: `c_marker` `{m}` is not a C identifier");
            ensure!(matches!(site, Site::Pc { .. }), "{what}: `c_marker` names a source site for an original-PC rule");
        }
        let request = match (self.request.as_deref(), &self.request_group) {
            (None, None) => None,
            (Some("call"), None) => Some(Request::Call),
            (Some("call"), Some(_)) => bail!("{what}: `request_group` is for keyed requests, not `call`"),
            (Some(k), Some(g)) => Some(Request::Keyed { key: value(k).with_context(|| format!("{what}: field `request`"))?, group: g.clone() }),
            (Some(_), None) => bail!("{what}: a keyed `request` needs `request_group`"),
            (None, Some(_)) => bail!("{what}: `request_group` without a keyed `request`"),
        };
        let target = match self.placement.as_deref() {
            None => None,
            Some("selector") => {
                let value = need("selector", src("selector", &self.selector)?)?;
                ensure!(!self.cases.is_empty(), "{what}: field `cases` is required when placement = \"selector\"");
                let mut cases = Vec::new();
                for (k, p) in &self.cases {
                    let v = crate::parse_u32(k).with_context(|| format!("{what}: case `{k}` is not a u32"))?;
                    ensure!(placements.contains(p.as_str()), "{what}: case `{k}` names no placement `{p}`");
                    cases.push((v, p.clone()));
                }
                cases.sort();
                ensure!(cases.windows(2).all(|w| w[0].0 != w[1].0), "{what}: two cases have the same value");
                Some(Target::Selector { value, cases })
            }
            Some("request") => {
                ensure!(request.is_some(), "{what}: placement = \"request\" needs a `request`");
                Some(Target::Request)
            }
            Some(p) => {
                ensure!(placements.contains(p), "{what}: no placement `{p}`");
                Some(Target::Literal(p.to_string()))
            }
        };
        if !matches!(target, Some(Target::Selector { .. })) {
            ensure!(self.selector.is_none() && self.cases.is_empty(), "{what}: `selector` and `cases` need placement = \"selector\"");
        }
        let destination = src("destination", &self.destination)?;
        let length = src("length", &self.length)?;
        let source_offset = match self.source_offset.as_deref() {
            None => None,
            Some("placement") => Some(Offset::Placement),
            Some(v) => Some(Offset::Value(value(v).with_context(|| format!("{what}: field `source_offset`"))?)),
        };
        let success = self.success.unwrap_or_default();
        match self.effect {
            Effect::Publish => {
                ensure!(target.is_some(), "{what}: field `placement` is required for effect Publish");
                need("destination", destination.clone())?;
                need("length", length.clone())?;
                ensure!(source_offset.is_some(), "{what}: field `source_offset` is required for effect Publish");
            }
            Effect::Invalidate => {
                need("destination", destination.clone())?;
                need("length", length.clone())?;
                absent("placement", target.is_some())?;
                absent("source_offset", source_offset.is_some())?;
            }
            Effect::Begin => {
                ensure!(target.is_some() && !matches!(target, Some(Target::Request)), "{what}: effect Begin captures a literal or selected `placement`");
                ensure!(request.is_some(), "{what}: field `request` is required for effect Begin");
            }
            Effect::End => {
                ensure!(request.is_some(), "{what}: field `request` is required for effect End");
                absent("placement", target.is_some())?;
                absent("source_offset", source_offset.is_some())?;
            }
        }
        // What each event makes available.
        let mut values: Vec<(&str, &Value)> = Vec::new();
        values.extend(destination.iter().map(|v| ("destination", v)));
        values.extend(length.iter().map(|v| ("length", v)));
        if let Some(Offset::Value(v)) = &source_offset {
            values.push(("source_offset", v));
        }
        if let Some(Target::Selector { value, .. }) = &target {
            values.push(("selector", value));
        }
        if let Some(Request::Keyed { key, .. }) = &request {
            values.push(("request", key));
        }
        for (field, v) in values {
            match v {
                Value::Return => ensure!(self.event == Event::Return, "{what}: `{field}` reads `return`, which only event `return` has"),
                Value::WriteAddress | Value::WriteSize => {
                    ensure!(self.event == Event::AfterWrite, "{what}: `{field}` reads a write, which only event `after-write` has")
                }
                _ => {}
            }
        }
        ensure!(
            success == Success::Always || self.event == Event::Return,
            "{what}: success = {success:?} tests the operation's result, which only event `return` has"
        );
        Ok(RuleFacts {
            id: self.id.clone(),
            site,
            event: self.event,
            c_marker: self.c_marker.clone(),
            effect: self.effect,
            target,
            destination,
            length,
            source_offset,
            success,
            request,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[dispatch]
schema_version = 1
require_explicit = false

[[dispatch.placements]]
id = "slot-a"
container = "module_a"

[[dispatch.placements]]
id = "slot-b"
container = "module_b"

[[dispatch.rules]]
id = "load-completed"
container = "resident"
function = "load_module"
event = "return"
effect = "publish"
placement = "selector"
selector = "arg:0"
cases = { "0" = "slot-a", "1" = "slot-b" }
destination = "arg:1"
length = "arg:2"
source_offset = "const:0"
success = "zero"
request = "call"
"#;

    fn err(text: &str) -> String {
        match parse(text).and_then(|d| d.check()) {
            Ok(_) => panic!("accepted:\n{text}"),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn the_plan_example_parses() {
        let facts = parse(EXAMPLE).unwrap().check().unwrap();
        let r = &facts.rules[0];
        assert_eq!(r.site, Site::Function { container: "resident".into(), function: "load_module".into() });
        assert_eq!(r.success, Success::Zero);
        assert_eq!(r.request, Some(Request::Call));
        assert_eq!(
            r.target,
            Some(Target::Selector { value: Value::Arg(0), cases: vec![(0, "slot-a".into()), (1, "slot-b".into())] })
        );
        assert_eq!(r.source_offset, Some(Offset::Value(Value::Const(0))));
    }

    #[test]
    fn value_sources() {
        assert_eq!(value("const:0x80").unwrap(), Value::Const(0x80));
        assert_eq!(value("field:1:8").unwrap(), Value::Field { arg: 1, offset: 8 });
        assert_eq!(value("global:main:D_80010000").unwrap(), Value::Global { container: "main".into(), symbol: "D_80010000".into() });
        assert!(value("arg:99").is_err());
        assert!(value("expr:a0+4").is_err());
        assert!(value("return:1").is_err());
    }

    #[test]
    fn facts_fail_with_the_rule_and_field() {
        let e = err(&EXAMPLE.replace("require_explicit = false", "require_explicit = false\nextra = 1"));
        assert!(e.contains("unknown field"), "{e}");
        let e = err(&EXAMPLE.replace("id = \"slot-b\"", "id = \"slot-a\""));
        assert!(e.contains("two placements have id `slot-a`"), "{e}");
        let e = err(&EXAMPLE.replace("length = \"arg:2\"\n", ""));
        assert!(e.contains("rule `load-completed`") && e.contains("`length`"), "{e}");
        let e = err(&EXAMPLE.replace("\"1\" = \"slot-b\"", "\"1\" = \"slot-c\""));
        assert!(e.contains("names no placement `slot-c`"), "{e}");
        let e = err(&EXAMPLE.replace("event = \"return\"", "event = \"enter\""));
        assert!(e.contains("only event `return` has"), "{e}");
        let e = err(&EXAMPLE.replace("function = \"load_module\"", "function = \"load_module\"\npc = 0x80010000"));
        assert!(e.contains("exactly one site"), "{e}");
        let e = err(&EXAMPLE.replace("request = \"call\"", "request = \"arg:3\""));
        assert!(e.contains("request_group"), "{e}");
        let e = err(&EXAMPLE.replace("schema_version = 1", "schema_version = 2"));
        assert!(e.contains("schema_version 2"), "{e}");
    }

    #[test]
    fn markers_are_declared_for_authored_c() {
        let text = format!(
            "{EXAMPLE}\n[[dispatch.rules]]\nid = \"chunk\"\ncontainer = \"resident\"\npc = 0x80010040\nevent = \"after-write\"\n\
             c_marker = \"chunk_written\"\neffect = \"invalidate\"\ndestination = \"write-address\"\nlength = \"write-size\"\n"
        );
        let h = header(&parse(&text).unwrap());
        assert!(h.contains("void psxw_binding_site_chunk_written(psxw_binding_u32 destination, psxw_binding_u32 length,"), "{h}");
        assert_eq!(h.matches("psxw_binding_site_").count(), 1);
    }

    #[test]
    fn invalidation_asserts_no_identity() {
        let text = EXAMPLE.replace("effect = \"publish\"", "effect = \"invalidate\"");
        assert!(err(&text).contains("`placement` does not apply"));
    }
}
