//! Grammar-as-data: a handler's argument surface declared ONCE, from which
//! everything model-facing derives — the JSON Schema, the JSON→flat-string
//! adapter, and the generated "what can I do" prose. The hand-written parser
//! keeps accepting the flat form (it also handles human aliases and loose
//! phrasing); a per-handler drift test pins the grammar to the parser so the
//! two can never disagree silently.
//!
//! WHY. The agent's tool surface used to be authored per handler: a `json!`
//! schema mirroring the parser's verb table, plus an `*_args_to_flat` adapter
//! mirroring it again — the same information written three times, which is the
//! duplicate-decider smell. Declaring the grammar as data collapses all three
//! into one declaration, and lets the registry PROJECT handlers into a small
//! set of model-facing group tools (`files`, `personal`, `system`, …): ~9
//! tools instead of ~45, which is under every published tool-count accuracy
//! cliff, and stable enough to send whole every turn (a byte-stable prefix is
//! what makes provider prompt caching work).
//!
//! A handler without a grammar still works — it is exposed as a standalone
//! tool exactly as before. Migration is incremental by construction.

use serde_json::{Map, Value, json};

/// What an operand's value looks like, and how it renders into the flat form.
#[derive(Clone, Copy, Debug)]
pub enum ArgKind {
    /// Free text (may contain spaces). Renders as-is.
    Text,
    /// An integer. Renders as its decimal form.
    Int,
    /// A flag: renders `flag` when true, nothing when false/absent.
    Bool { flag: &'static str },
    /// One of a fixed set. The schema constrains the model to the set.
    Choice(&'static [&'static str]),
    /// A list of strings. Renders space-joined (so items must not contain
    /// spaces — the flat grammars this feeds are whitespace-split).
    List,
}

/// One argument of a verb, in flat-form order.
#[derive(Clone, Copy, Debug)]
pub struct Operand {
    /// JSON property name AND schema field name. Specific over generic
    /// (`archive`, not `input`).
    pub name: &'static str,
    /// Model-facing description: expected format, examples, when to omit.
    pub desc: &'static str,
    pub required: bool,
    pub kind: ArgKind,
    /// Literal emitted before the value when the value is present — this is
    /// how flat grammars like `<archive> to <dest>` are expressed
    /// (`prefix: Some("to")` on the `dest` operand).
    pub prefix: Option<&'static str>,
}

/// One action a handler supports. `name: ""` declares a free-form handler
/// (single implicit action; the flat form is just its operands).
#[derive(Clone, Copy, Debug)]
pub struct Verb {
    pub name: &'static str,
    /// One line: what it does + when to use it. This is model-facing judgment
    /// prose — the one part that cannot be derived, so write it well.
    pub desc: &'static str,
    pub operands: &'static [Operand],
    /// Whether THIS action changes state (files, system, stored data). Drives
    /// the per-turn one-mutating-call hold and approval weighting at action
    /// granularity, replacing the handler-wide flag for grouped tools.
    pub mutates: bool,
}

/// A handler's declared argument surface.
#[derive(Clone, Copy, Debug)]
pub struct Grammar {
    pub verbs: &'static [Verb],
}

impl Grammar {
    /// Whether this is a single free-form action (`verbs == [Verb{name: "", ..}]`).
    pub fn is_free_form(&self) -> bool {
        self.verbs.len() == 1 && self.verbs[0].name.is_empty()
    }

    /// Find a verb by name ("" for the free form).
    pub fn verb(&self, name: &str) -> Option<&'static Verb> {
        self.verbs.iter().find(|v| v.name == name)
    }

    /// Render a structured call back to the flat string the parser accepts:
    /// the verb name, then each operand in declared order (prefix + value),
    /// skipping absent/empty/false values. Values are read from `args` by
    /// operand name and accept the JSON type or its string spelling.
    pub fn to_flat(&self, verb: &Verb, args: &Map<String, Value>) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !verb.name.is_empty() {
            parts.push(verb.name.to_string());
        }
        for op in verb.operands {
            let v = args.get(op.name);
            match op.kind {
                ArgKind::Bool { flag } => {
                    if matches!(v, Some(Value::Bool(true)))
                        || matches!(v, Some(Value::String(s)) if s == "true")
                    {
                        parts.push(flag.to_string());
                    }
                }
                ArgKind::List => {
                    let joined = match v {
                        Some(Value::Array(items)) => items
                            .iter()
                            .map(|i| match i {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                        Some(Value::String(s)) => s.clone(),
                        _ => String::new(),
                    };
                    if !joined.trim().is_empty() {
                        if let Some(p) = op.prefix {
                            parts.push(p.to_string());
                        }
                        parts.push(joined.trim().to_string());
                    }
                }
                ArgKind::Text | ArgKind::Int | ArgKind::Choice(_) => {
                    let s = match v {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Number(n)) => n.to_string(),
                        Some(Value::Bool(b)) => b.to_string(),
                        _ => String::new(),
                    };
                    if !s.trim().is_empty() {
                        if let Some(p) = op.prefix {
                            parts.push(p.to_string());
                        }
                        parts.push(s.trim().to_string());
                    }
                }
            }
        }
        parts.join(" ")
    }

    /// Flatten a handler-level structured call (the JSON shape
    /// [`Grammar::handler_schema`] declares: bare verb in `action`, operands
    /// by name) to the flat string the parser accepts. `None` when `args` is
    /// not a JSON object or names no verb this grammar has — callers keep the
    /// raw string so a flat/legacy caller passes through untouched. This is
    /// the ONE structured→flat decider; a handler's `*_args_to_flat` should
    /// delegate here rather than re-implement the walk.
    pub fn flatten_json(&self, args: &str) -> Option<String> {
        let t = args.trim();
        if !t.starts_with('{') {
            return None;
        }
        let parsed: Value = serde_json::from_str(t).ok()?;
        let map = parsed.as_object()?;
        let verb = if self.is_free_form() {
            self.verb("")?
        } else {
            self.verb(map.get("action")?.as_str()?)?
        };
        Some(self.to_flat(verb, map))
    }

    /// The standalone (single-handler) JSON Schema this grammar implies —
    /// the default `ActionHandler::input_schema` derives from this, replacing
    /// every hand-written `*_input_schema()`.
    pub fn handler_schema(&self) -> Value {
        if self.is_free_form() {
            let verb = &self.verbs[0];
            let mut props = Map::new();
            let mut required: Vec<Value> = Vec::new();
            for op in verb.operands {
                props.insert(op.name.to_string(), operand_schema(op, None));
                if op.required {
                    required.push(json!(op.name));
                }
            }
            return json!({
                "type": "object",
                "properties": props,
                "required": required,
                "additionalProperties": false
            });
        }
        let actions: Vec<&str> = self.verbs.iter().map(|v| v.name).collect();
        let mut props = Map::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": actions,
                "description": action_lines(self.verbs.iter().map(|v| (v.name.to_string(), v))),
            }),
        );
        merge_operand_props(
            &mut props,
            self.verbs.iter().map(|v| (v.name.to_string(), *v)),
        );
        json!({
            "type": "object",
            "properties": props,
            "required": ["action"],
            "additionalProperties": false
        })
    }
}

/// The JSON Schema for a GROUP tool: one `action` enum over every member
/// handler's compound actions, plus the union of their operand fields (merged
/// by name+type, annotated with the actions that read them). `actions` pairs
/// each compound action name with its verb, in the stable order the catalog
/// was assembled in.
pub fn group_schema(actions: &[(String, Verb)]) -> Value {
    let names: Vec<&str> = actions.iter().map(|(n, _)| n.as_str()).collect();
    let mut props = Map::new();
    props.insert(
        "action".into(),
        json!({
            "type": "string",
            "enum": names,
            "description": action_lines(actions.iter().map(|(n, v)| (n.clone(), v))),
        }),
    );
    merge_operand_props(&mut props, actions.iter().cloned());
    json!({
        "type": "object",
        "properties": props,
        "required": ["action"],
        "additionalProperties": false
    })
}

/// The schema fragment for one operand. `used_by` (for group schemas) prepends
/// which actions read the field, so a merged field stays legible.
fn operand_schema(op: &Operand, used_by: Option<&str>) -> Value {
    let ty = match op.kind {
        ArgKind::Int => "integer",
        ArgKind::Bool { .. } => "boolean",
        ArgKind::List => "array",
        _ => "string",
    };
    let desc = match used_by {
        Some(actions) => format!("[for {actions}] {}", wire_desc(op.desc, OPERAND_DESC_CAP)),
        None => wire_desc(op.desc, OPERAND_DESC_CAP).to_string(),
    };
    let mut s = Map::new();
    s.insert("type".into(), json!(ty));
    s.insert("description".into(), json!(desc));
    if let ArgKind::Choice(values) = op.kind {
        s.insert("enum".into(), json!(values));
    }
    if matches!(op.kind, ArgKind::List) {
        s.insert("items".into(), json!({"type": "string"}));
    }
    Value::Object(s)
}

/// The wire rendering of a description: its FIRST sentence, word-capped.
///
/// Verb/operand descs are written as full judgment prose (good documentation,
/// and the Guide can use all of it), but the schema ships with every request —
/// on a token-budgeted provider (Groq free tier ≈ 8k tokens/request) the
/// uncut catalog alone blew the whole budget. The first sentence carries the
/// "what/when"; formats and caveats belong in the OPERAND descs, which are
/// shipped whole (they are what argument accuracy hangs on).
fn wire_desc(s: &str, cap: usize) -> &str {
    let first = match s.find(". ") {
        Some(i) => &s[..i + 1],
        None => s,
    };
    if first.len() <= cap {
        return first;
    }
    // Over-long single sentence: cut at the last word boundary under the cap.
    match first[..cap].rfind(' ') {
        Some(i) => &first[..i],
        None => &first[..cap],
    }
}

/// Action lines get a tight cap — the compound NAMES already carry most of
/// the signal (`note_add`, `win_close`); operand descs get a looser one, since
/// formats and examples (what argument accuracy hangs on) live there.
const ACTION_DESC_CAP: usize = 100;
const OPERAND_DESC_CAP: usize = 200;

/// "`action` — desc" lines for an action property's description. First
/// sentence per action (see [`wire_desc`]).
fn action_lines<'a>(verbs: impl Iterator<Item = (String, &'a Verb)>) -> String {
    let mut out = String::from("Which operation to perform:\n");
    for (name, v) in verbs {
        out.push_str(&format!(
            "- `{name}`: {}\n",
            wire_desc(v.desc, ACTION_DESC_CAP)
        ));
    }
    out.trim_end().to_string()
}

/// Merge operand fields from several (action-name, verb) pairs into `props`.
/// Same name + same JSON type merge into one field annotated with every action
/// that reads it; a type conflict keeps both under action-prefixed names.
fn merge_operand_props(
    props: &mut Map<String, Value>,
    verbs: impl Iterator<Item = (String, Verb)>,
) {
    // (field name, type tag, actions using it, operand)
    let mut fields: Vec<(String, &'static str, Vec<String>, Operand)> = Vec::new();
    for (action, verb) in verbs {
        for op in verb.operands {
            let ty = match op.kind {
                ArgKind::Int => "integer",
                ArgKind::Bool { .. } => "boolean",
                ArgKind::List => "array",
                _ => "string",
            };
            match fields
                .iter_mut()
                .find(|(name, t, _, _)| name == op.name && *t == ty)
            {
                Some((_, _, actions, _)) => actions.push(action.clone()),
                None => {
                    let clash = fields.iter().any(|(name, _, _, _)| name == op.name);
                    let name = if clash {
                        format!("{action}_{}", op.name)
                    } else {
                        op.name.to_string()
                    };
                    fields.push((name, ty, vec![action.clone()], *op));
                }
            }
        }
    }
    for (name, _, actions, op) in fields {
        // Cap the users-of-this-field annotation: past a few names it stops
        // informing and starts costing (a field shared by 14 actions listed
        // them all, per request, per turn).
        let used_by = if actions.len() <= 3 {
            actions.join(", ")
        } else {
            format!("{} +{} more", actions[..3].join(", "), actions.len() - 3)
        };
        props.insert(name, operand_schema(&op, Some(&used_by)));
    }
}

// ── Model-facing tool groups ─────────────────────────────────────────────────

/// The model-facing family a handler's actions surface under. Deliberately NOT
/// [`super::CommandCategory`] (a Guide/UI grouping): the model wants few,
/// cohesive tools, and the UI's `Utilities` is an 18-handler grab-bag.
/// `Standalone` keeps a handler as its own tool (`run`), and every handler
/// WITHOUT a grammar is standalone regardless.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolGroup {
    /// Browse, open, search, archive, and transform files and projects.
    Files,
    /// Web search, URLs, YouTube, definitions, bookmarks, quicklinks.
    Web,
    /// Desktop + system control: apps, windows, audio, network, power,
    /// services, packages, screenshots, system info.
    System,
    /// Media playback control.
    Media,
    /// Developer utilities: encoders, hashes, ssh, user scripts.
    Dev,
    /// The user's own data: notes, todos, reminders, timers, snippets,
    /// clipboard history, aliases, pins.
    Personal,
    /// Instant lookups + generators: calc, time, weather, emoji, color, QR…
    Utils,
    /// Not grouped — exposed as its own tool.
    Standalone,
}

impl ToolGroup {
    /// The model-facing tool name.
    pub fn tool_name(self) -> &'static str {
        match self {
            ToolGroup::Files => "files",
            ToolGroup::Web => "web_tools",
            ToolGroup::System => "system_control",
            ToolGroup::Media => "media_control",
            ToolGroup::Dev => "dev_tools",
            ToolGroup::Personal => "personal_data",
            ToolGroup::Utils => "quick_tools",
            ToolGroup::Standalone => "",
        }
    }

    /// The model-facing tool description — the judgment prose for the group.
    pub fn description(self) -> &'static str {
        match self {
            ToolGroup::Files => {
                "Work with files, folders, and projects on this machine: browse \
                 directories, open files, open a project in its editor, zip/extract \
                 archives, convert or resize images. Pick the action that matches the task."
            }
            ToolGroup::Web => {
                "Web actions: search the web, open a URL, search YouTube, look up a word, \
                 open a bookmark or quicklink. Use for anything that ends in a browser."
            }
            ToolGroup::System => {
                "Control this Linux desktop and system: launch/quit/focus apps, manage \
                 windows, volume/brightness/wifi/bluetooth/power, systemd services, \
                 install or search packages, take screenshots, read system info."
            }
            ToolGroup::Media => {
                "Control media playback (play/pause/next/previous, what's playing) via \
                 MPRIS — works with Spotify, browsers, and local players."
            }
            ToolGroup::Dev => {
                "Developer utilities: encode/decode (base64, URL), hash, format JSON, \
                 UUIDs, ssh to saved hosts, run the user's saved scripts, inspect the \
                 launcher's own context."
            }
            ToolGroup::Personal => {
                "The user's personal data in this launcher: notes, todos, reminders, \
                 timers, snippets, clipboard history, command aliases, pinned items. \
                 Reads are instant; writes persist."
            }
            ToolGroup::Utils => {
                "Instant answers and generators: calculator, unit/time-zone conversion, \
                 weather, emoji/symbol/unicode search, color conversion, QR codes, \
                 passwords and random values."
            }
            ToolGroup::Standalone => "",
        }
    }

    /// Every group that can surface as a model tool.
    pub const ALL: &'static [ToolGroup] = &[
        ToolGroup::Files,
        ToolGroup::Web,
        ToolGroup::System,
        ToolGroup::Media,
        ToolGroup::Dev,
        ToolGroup::Personal,
        ToolGroup::Utils,
    ];
}

/// The compound action name a handler's verb surfaces as inside a group tool:
/// the handler id for a free-form/single grammar, `{id}_{verb}` otherwise.
pub fn compound_action(handler_id: &str, verb: &Verb) -> String {
    if verb.name.is_empty() {
        handler_id.to_string()
    } else {
        format!("{handler_id}_{}", verb.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXTRACT: Grammar = Grammar {
        verbs: &[Verb {
            name: "",
            desc: "Extract an archive",
            mutates: true,
            operands: &[
                Operand {
                    name: "archive",
                    desc: "Path to the archive",
                    required: true,
                    kind: ArgKind::Text,
                    prefix: None,
                },
                Operand {
                    name: "destination",
                    desc: "Where to extract",
                    required: false,
                    kind: ArgKind::Text,
                    prefix: Some("to"),
                },
            ],
        }],
    };

    const NOTES: Grammar = Grammar {
        verbs: &[
            Verb {
                name: "add",
                desc: "Add a note",
                mutates: true,
                operands: &[Operand {
                    name: "text",
                    desc: "The note text",
                    required: true,
                    kind: ArgKind::Text,
                    prefix: None,
                }],
            },
            Verb {
                name: "delete",
                desc: "Delete a note by id",
                mutates: true,
                operands: &[Operand {
                    name: "id",
                    desc: "The note id",
                    required: true,
                    kind: ArgKind::Int,
                    prefix: None,
                }],
            },
        ],
    };

    const DEV: Grammar = Grammar {
        verbs: &[Verb {
            name: "base64",
            desc: "Base64 encode/decode",
            mutates: false,
            operands: &[
                Operand {
                    name: "decode",
                    desc: "Decode instead of encode",
                    required: false,
                    kind: ArgKind::Bool { flag: "-d" },
                    prefix: None,
                },
                Operand {
                    name: "text",
                    desc: "The text",
                    required: true,
                    kind: ArgKind::Text,
                    prefix: None,
                },
            ],
        }],
    };

    fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn free_form_flat_renders_prefix_only_when_present() {
        let v = EXTRACT.verb("").unwrap();
        assert_eq!(
            EXTRACT.to_flat(
                v,
                &args(&[("archive", json!("a.zip")), ("destination", json!("out"))])
            ),
            "a.zip to out"
        );
        assert_eq!(
            EXTRACT.to_flat(v, &args(&[("archive", json!("a.zip"))])),
            "a.zip"
        );
    }

    #[test]
    fn verb_flat_renders_verb_then_operands_in_order() {
        let v = NOTES.verb("add").unwrap();
        assert_eq!(
            NOTES.to_flat(v, &args(&[("text", json!("buy milk"))])),
            "add buy milk"
        );
        let d = NOTES.verb("delete").unwrap();
        // Numbers arrive as JSON numbers or strings; both render.
        assert_eq!(NOTES.to_flat(d, &args(&[("id", json!(3))])), "delete 3");
        assert_eq!(NOTES.to_flat(d, &args(&[("id", json!("3"))])), "delete 3");
    }

    #[test]
    fn bool_operand_renders_its_flag_only_when_true() {
        let v = DEV.verb("base64").unwrap();
        assert_eq!(
            DEV.to_flat(
                v,
                &args(&[("decode", json!(true)), ("text", json!("aGk="))])
            ),
            "base64 -d aGk="
        );
        assert_eq!(
            DEV.to_flat(v, &args(&[("decode", json!(false)), ("text", json!("hi"))])),
            "base64 hi"
        );
    }

    #[test]
    fn list_operand_joins_items_with_spaces() {
        const ZIP: Grammar = Grammar {
            verbs: &[Verb {
                name: "",
                desc: "Zip files",
                mutates: true,
                operands: &[
                    Operand {
                        name: "paths",
                        desc: "Files to zip",
                        required: true,
                        kind: ArgKind::List,
                        prefix: None,
                    },
                    Operand {
                        name: "output",
                        desc: "Output archive",
                        required: false,
                        kind: ArgKind::Text,
                        prefix: Some("to"),
                    },
                ],
            }],
        };
        let v = ZIP.verb("").unwrap();
        assert_eq!(
            ZIP.to_flat(
                v,
                &args(&[
                    ("paths", json!(["a.txt", "b.txt"])),
                    ("output", json!("o.zip"))
                ])
            ),
            "a.txt b.txt to o.zip"
        );
    }

    #[test]
    fn handler_schema_free_form_has_operand_fields() {
        let s = EXTRACT.handler_schema();
        assert_eq!(s["properties"]["archive"]["type"], "string");
        assert_eq!(s["required"], json!(["archive"]));
        assert_eq!(s["additionalProperties"], json!(false));
    }

    #[test]
    fn handler_schema_verbed_constrains_action_enum() {
        let s = NOTES.handler_schema();
        assert_eq!(s["properties"]["action"]["enum"], json!(["add", "delete"]));
        assert_eq!(s["required"], json!(["action"]));
        // Merged operand fields present.
        assert_eq!(s["properties"]["text"]["type"], "string");
        assert_eq!(s["properties"]["id"]["type"], "integer");
    }

    #[test]
    fn merged_fields_note_which_actions_use_them() {
        let s = NOTES.handler_schema();
        let desc = s["properties"]["text"]["description"].as_str().unwrap();
        assert!(desc.contains("[for add]"), "{desc}");
    }

    #[test]
    fn same_name_different_type_gets_action_prefixed() {
        const G: Grammar = Grammar {
            verbs: &[
                Verb {
                    name: "a",
                    desc: "a",
                    mutates: false,
                    operands: &[Operand {
                        name: "value",
                        desc: "text value",
                        required: false,
                        kind: ArgKind::Text,
                        prefix: None,
                    }],
                },
                Verb {
                    name: "b",
                    desc: "b",
                    mutates: false,
                    operands: &[Operand {
                        name: "value",
                        desc: "numeric value",
                        required: false,
                        kind: ArgKind::Int,
                        prefix: None,
                    }],
                },
            ],
        };
        let s = G.handler_schema();
        assert!(s["properties"]["value"].is_object());
        assert!(s["properties"]["b_value"].is_object(), "{s}");
    }

    #[test]
    fn flatten_json_resolves_verb_and_passes_raw_through() {
        assert_eq!(
            NOTES.flatten_json(r#"{"action":"add","text":"buy milk"}"#),
            Some("add buy milk".to_string())
        );
        assert_eq!(
            EXTRACT.flatten_json(r#"{"archive":"a.zip"}"#),
            Some("a.zip".to_string())
        );
        // Flat/legacy callers and unknown verbs stay untouched (caller keeps raw).
        assert_eq!(NOTES.flatten_json("add buy milk"), None);
        assert_eq!(NOTES.flatten_json(r#"{"action":"nope"}"#), None);
        assert_eq!(NOTES.flatten_json("{not json"), None);
    }

    #[test]
    fn compound_action_names() {
        assert_eq!(
            compound_action("extract", EXTRACT.verb("").unwrap()),
            "extract"
        );
        assert_eq!(
            compound_action("note", NOTES.verb("add").unwrap()),
            "note_add"
        );
    }
}
