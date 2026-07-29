//! Read-side rendering and projection: turning a resolved node/epic (or a set
//! of them) into the output forms every read command speaks.
//!
//! One rule anchors this module: the JSON form is the canonical source of
//! truth, and the human forms are renderings of it. So `show`/`list` first
//! build a [`serde_json::Value`] with every field (including computed ones like
//! an epic's state), and the markdown/plain-text/raw/tab forms are derived from
//! that same value. Field projection (`--field`/`--fields`) is a walk over that
//! value by dotted leaf path, so it behaves identically across all three modes.
//!
//! Everything here returns owned `String`s and takes no I/O and no terminal:
//! callers resolve the data and decide colour/streaming. That keeps rendering
//! unit-testable without a store or a TTY. Colour is never baked in here; the
//! plain-text list may carry ANSI only when a caller explicitly asks for it,
//! and the machine forms (json/ndjson/raw) are never coloured.

use std::fmt::Write as _;

use serde_json::{json, Map, Value};

use crate::domain::{epic_status, NodeStatus};
use crate::model::{Asset, Claim, Comment, EpicFile, NodeFile};

/// Why a read/projection could not be rendered as asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    /// A `--raw` selection did not resolve to exactly one value per line. Raw is
    /// strict-unambiguous: a whole structured or repeated field has no single
    /// unambiguous line form, so it is refused and the caller is pointed at the
    /// canonical JSON instead.
    #[error(
        "'{path}' is not a single value ({count} values); \
         --raw needs one value per line — use --json for structured output"
    )]
    RawAmbiguous {
        /// The offending dotted field path.
        path: String,
        /// How many values it resolved to.
        count: usize,
    },
    /// A dotted field path did not match any leaf in the value.
    #[error("no field '{0}' here")]
    UnknownField(String),
    /// A heavy/structured field was requested on `list`, which only serves
    /// summary/listable fields. Those fields are `show`-only.
    #[error("field '{field}' is not available on list; it is shown by 'show' — {listable}")]
    FieldNotListable {
        /// The offending field.
        field: String,
        /// A hint at what list does serve.
        listable: String,
    },
}

// ===========================================================================
// canonical JSON value model
// ===========================================================================

/// Build the canonical JSON value for a node: every stored field plus its
/// `ref`. This is the source of truth the other node forms render from.
pub fn node_to_json(epic_id: &str, node: &NodeFile) -> Value {
    let fm = &node.frontmatter;
    let mut map = Map::new();
    map.insert("ref".into(), json!(format!("{}/{}", epic_id, fm.number)));
    map.insert("epic".into(), json!(epic_id));
    map.insert("number".into(), json!(fm.number));
    map.insert("name".into(), json!(fm.name));
    map.insert("summary".into(), json!(fm.summary));
    map.insert("status".into(), json!(fm.status.wire_name()));
    map.insert("labels".into(), json!(fm.labels));
    map.insert(
        "parent".into(),
        match fm.parent {
            Some(p) => json!(format!("{}/{}", epic_id, p)),
            None => Value::Null,
        },
    );
    map.insert("blocked-by".into(), json!(fm.blocked_by));
    map.insert(
        "block-reason".into(),
        opt_string_json(fm.block_reason.as_deref()),
    );
    map.insert(
        "close-reason".into(),
        opt_string_json(fm.close_reason.as_deref()),
    );
    map.insert("claim".into(), claim_json(fm.claim.as_ref()));
    map.insert("assets".into(), assets_json(&fm.assets));
    map.insert("comments".into(), comments_json(&fm.comments));
    map.insert("created".into(), json!(fm.created.to_string()));
    map.insert("updated".into(), json!(fm.updated.to_string()));
    map.insert("body".into(), json!(node.body));
    merge_extra(&mut map, &fm.extra);
    Value::Object(map)
}

/// Build the canonical JSON value for an epic: every stored field plus its
/// computed `state` (which the stored form never carries — it is derived from
/// the closed flag and the node states injected here).
pub fn epic_to_json(epic: &EpicFile, node_statuses: &[NodeStatus]) -> Value {
    let fm = &epic.frontmatter;
    let mut map = Map::new();
    map.insert("id".into(), json!(fm.id));
    map.insert("name".into(), json!(fm.name));
    map.insert("summary".into(), json!(fm.summary));
    // `status` is computed, never stored: closed flag wins, else completed when
    // every node is terminal, else open (including an epic with no nodes).
    map.insert(
        "status".into(),
        json!(epic_status(fm.closed, node_statuses).wire_name()),
    );
    map.insert("closed".into(), json!(fm.closed));
    map.insert(
        "close-reason".into(),
        opt_string_json(fm.close_reason.as_deref()),
    );
    map.insert("labels".into(), json!(fm.labels));
    map.insert("next-number".into(), json!(fm.next_number));
    map.insert("nodes".into(), json!(node_statuses.len()));
    map.insert("assets".into(), assets_json(&fm.assets));
    map.insert("comments".into(), comments_json(&fm.comments));
    map.insert("created".into(), json!(fm.created.to_string()));
    map.insert("updated".into(), json!(fm.updated.to_string()));
    map.insert("body".into(), json!(epic.body));
    merge_extra(&mut map, &fm.extra);
    Value::Object(map)
}

/// A claim as JSON: `{ by, at }` when held, or null when unclaimed. `by` and
/// `at` always appear together — the machine form never shows a holder without
/// a timestamp.
fn claim_json(claim: Option<&Claim>) -> Value {
    match claim {
        Some(c) => json!({ "by": c.by, "at": c.at.to_string() }),
        None => Value::Null,
    }
}

fn assets_json(atts: &[Asset]) -> Value {
    Value::Array(
        atts.iter()
            .map(|a| {
                json!({
                    "name": a.name,
                    "description": opt_string_json(a.description.as_deref()),
                })
            })
            .collect(),
    )
}

/// Comments as JSON: every comment including soft-deleted ones. A deleted
/// comment carries `deleted: true` and its text is withheld (null) so the
/// machine form never leaks a tombstoned body.
fn comments_json(comments: &[Comment]) -> Value {
    Value::Array(
        comments
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "author": c.author.to_string(),
                    "created": c.created.to_string(),
                    "text": if c.deleted { Value::Null } else { json!(c.text) },
                    "deleted": c.deleted,
                })
            })
            .collect(),
    )
}

fn opt_string_json(s: Option<&str>) -> Value {
    match s {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

/// Fold the preserved unknown-key catch-all into the value map, so the JSON
/// form round-trips tolerated keys too. A known key is never overwritten by an
/// unknown one of the same name.
fn merge_extra(map: &mut Map<String, Value>, extra: &serde_yaml::Mapping) {
    for (k, v) in extra {
        if let Some(key) = k.as_str() {
            if !map.contains_key(key) {
                map.insert(key.to_string(), yaml_to_json(v));
            }
        }
    }
}

/// Convert a preserved YAML value into a JSON value for the canonical form.
fn yaml_to_json(v: &serde_yaml::Value) -> Value {
    match v {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => json!(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!(i)
            } else if let Some(u) = n.as_u64() {
                json!(u)
            } else if let Some(f) = n.as_f64() {
                json!(f)
            } else {
                Value::Null
            }
        }
        serde_yaml::Value::String(s) => json!(s),
        serde_yaml::Value::Sequence(seq) => Value::Array(seq.iter().map(yaml_to_json).collect()),
        serde_yaml::Value::Mapping(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                if let Some(key) = k.as_str() {
                    out.insert(key.to_string(), yaml_to_json(val));
                }
            }
            Value::Object(out)
        }
        // Tagged values are rare in frontmatter; render their inner value.
        serde_yaml::Value::Tagged(t) => yaml_to_json(&t.value),
    }
}

/// Serialise a JSON value in the canonical pretty form used by `--json`. Object
/// key order is preserved (we build maps in a deterministic order above), so
/// the output is stable across runs.
pub fn to_json_string(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string())
}

/// Serialise a JSON value compactly on one line, for `--ndjson`.
pub fn to_json_line(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

// ===========================================================================
// field-path projection (--field / --fields), shared by all modes
// ===========================================================================

/// Resolve a dotted leaf `path` against a JSON `value` to the sequence of leaf
/// values it selects, in document order. A path descends objects by key and
/// distributes over arrays: `comments.author` over an array of comments yields
/// one author per comment. A leaf value yields itself.
///
/// This never fails on shape; an unmatched path yields no values, which the
/// caller turns into an [`RenderError::UnknownField`] where that matters. Only
/// leaves are returned — descending onto an object or array that a further
/// segment does not resolve simply produces no leaf for that branch.
pub fn project_path(value: &Value, path: &str) -> Vec<Value> {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    let mut out = Vec::new();
    collect(value, &segments, &mut out);
    out
}

/// Walk `segments` from `value`, pushing every leaf reached onto `out`. An
/// array distributes the remaining path across its elements.
fn collect(value: &Value, segments: &[&str], out: &mut Vec<Value>) {
    match segments.split_first() {
        // No more path: this is a selected node. If it is a scalar it is a leaf;
        // an array of scalars distributes to one leaf per element; anything else
        // (object, nested array) is kept whole so the caller can judge it.
        None => match value {
            Value::Array(items) => {
                for item in items {
                    out.push(item.clone());
                }
            }
            other => out.push(other.clone()),
        },
        Some((&head, rest)) => match value {
            Value::Object(map) => {
                if let Some(child) = map.get(head) {
                    collect(child, rest, out);
                }
            }
            Value::Array(items) => {
                // Distribute the same remaining path across each element.
                for item in items {
                    collect(item, segments, out);
                }
            }
            _ => {}
        },
    }
}

/// Resolve a dotted `path` to its terminal value(s) *without* distributing a
/// terminal array, so a path that names a whole structured field yields that
/// field as a single structured value (which raw then judges ambiguous).
/// Intermediate arrays still distribute, so `comments.author` yields one author
/// per comment (the terminal `author` is a scalar), while a bare `comments` or
/// `assets` yields the array itself.
fn project_terminal(value: &Value, path: &str) -> Vec<Value> {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    let mut out = Vec::new();
    collect_terminal(value, &segments, &mut out);
    out
}

fn collect_terminal(value: &Value, segments: &[&str], out: &mut Vec<Value>) {
    match segments.split_first() {
        // Terminal: keep the value whole (arrays are not distributed here).
        None => out.push(value.clone()),
        Some((&head, rest)) => match value {
            Value::Object(map) => {
                if let Some(child) = map.get(head) {
                    collect_terminal(child, rest, out);
                }
            }
            // An intermediate array distributes the remaining path across its
            // elements, so descending through a repeated field fans out.
            Value::Array(items) => {
                for item in items {
                    collect_terminal(item, segments, out);
                }
            }
            _ => {}
        },
    }
}

/// Render a single JSON leaf as its raw one-line string. Strings render
/// unquoted; scalars render naturally; `null` renders empty. A structured value
/// (object or array) has no unambiguous one-line form and returns `None`.
fn leaf_to_raw(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some(String::new()),
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

// ===========================================================================
// show
// ===========================================================================

/// The projection requested on a `show`: nothing (the whole value), a single
/// path, or several paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Projection {
    /// No `--field`/`--fields`: operate on the whole value.
    #[default]
    Whole,
    /// A single dotted leaf path.
    One(String),
    /// Several dotted leaf paths.
    Many(Vec<String>),
}

impl Projection {
    /// The paths this projection selects, or empty for the whole value.
    fn paths(&self) -> Vec<String> {
        match self {
            Projection::Whole => Vec::new(),
            Projection::One(p) => vec![p.clone()],
            Projection::Many(ps) => ps.clone(),
        }
    }
}

/// Render `show --json`: the whole canonical value, or a projected sub-view.
///
/// With no projection the whole value is emitted. With one path the selected
/// leaf value(s) are emitted (a single value bare, several as an array). With
/// several paths an object keyed by path is emitted, each holding that path's
/// value(s). This keeps `--json` the faithful, machine-first form under
/// projection too.
pub fn show_json(value: &Value, projection: &Projection) -> Result<String, RenderError> {
    match projection {
        Projection::Whole => Ok(to_json_string(value)),
        Projection::One(path) => {
            let vals = project_path(value, path);
            if vals.is_empty() {
                return Err(RenderError::UnknownField(path.clone()));
            }
            let out = if vals.len() == 1 {
                vals.into_iter().next().unwrap()
            } else {
                Value::Array(vals)
            };
            Ok(to_json_string(&out))
        }
        Projection::Many(paths) => {
            let mut map = Map::new();
            for path in paths {
                let vals = project_path(value, path);
                if vals.is_empty() {
                    return Err(RenderError::UnknownField(path.clone()));
                }
                let entry = if vals.len() == 1 {
                    vals.into_iter().next().unwrap()
                } else {
                    Value::Array(vals)
                };
                map.insert(path.clone(), entry);
            }
            Ok(to_json_string(&Value::Object(map)))
        }
    }
}

/// Render `show --raw`: strict-unambiguous leaf values, one per line.
///
/// Raw operates on leaves. Every selected path must resolve to leaf scalars,
/// each rendered on its own line. A selection that resolves to a structured
/// value, or to nothing, is a hard error pointing at `--json` — raw never
/// guesses a flattening. Selecting the whole value (no projection) is likewise
/// ambiguous unless it is itself a single scalar.
pub fn show_raw(value: &Value, projection: &Projection) -> Result<String, RenderError> {
    let paths = projection.paths();
    let mut lines = Vec::new();

    if paths.is_empty() {
        // No projection: the whole value must itself be a single scalar leaf,
        // otherwise there is no unambiguous one-value-per-line rendering.
        match leaf_to_raw(value) {
            Some(line) => lines.push(line),
            None => {
                return Err(RenderError::RawAmbiguous {
                    path: "(whole)".to_string(),
                    count: count_leaves(value),
                })
            }
        }
    } else {
        for path in &paths {
            let vals = project_terminal(value, path);
            if vals.is_empty() {
                return Err(RenderError::UnknownField(path.clone()));
            }
            for v in &vals {
                match leaf_to_raw(v) {
                    Some(line) => lines.push(line),
                    // A structured terminal (a whole array/object field) has no
                    // unambiguous one-value-per-line form — point at --json.
                    None => {
                        return Err(RenderError::RawAmbiguous {
                            path: path.clone(),
                            count: count_leaves(v),
                        })
                    }
                }
            }
        }
    }
    Ok(lines.join("\n"))
}

/// A rough leaf count for an error message: how many scalar values are buried
/// in a structured value, so the "N values" hint is meaningful.
fn count_leaves(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.iter().map(count_leaves).sum(),
        Value::Object(map) => map.values().map(count_leaves).sum(),
        _ => 1,
    }
}

/// Whether a node/epic is a node (`true`) or an epic, inferred from the
/// canonical value's shape (a node carries `ref`, an epic carries `id` with no
/// `ref`). Used only to pick the markdown heading vocabulary.
fn is_node_value(value: &Value) -> bool {
    value.get("ref").is_some()
}

/// A direct-children row for the markdown table: reference, name, status.
#[derive(Debug, Clone)]
pub struct ChildRow {
    /// The child's `<epic-id>/<n>` reference.
    pub reference: String,
    /// Its one-line name.
    pub name: String,
    /// Its state's wire name.
    pub status: String,
}

/// Render `show --markdown` (the default, viewer-friendly form): everything in
/// order — a metadata table, the name as an H1, the summary as a blockquote, a
/// direct-children table, an assets table, the verbatim body, then comments.
///
/// `children` are the direct children to tabulate (a node's child nodes, or an
/// epic's top-level nodes); the caller resolves them. Comments render live
/// entries in full and are otherwise hidden — a tombstone form is only shown
/// when the caller passes deleted entries in `comment_views`.
pub fn show_markdown(
    value: &Value,
    children: &[ChildRow],
    comment_views: &[CommentLine],
) -> String {
    let mut s = String::new();
    let node = is_node_value(value);

    // -- metadata table -----------------------------------------------------
    s.push_str("| field | value |\n|---|---|\n");
    let meta_keys: &[&str] = if node {
        &[
            "ref", "number", "status", "parent", "labels", "created", "updated",
        ]
    } else {
        &["id", "status", "labels", "nodes", "created", "updated"]
    };
    for key in meta_keys {
        if let Some(v) = value.get(*key) {
            let _ = writeln!(s, "| {} | {} |", key, meta_cell(v));
        }
    }
    // blocked-by (dependency list) and block-reason are worth surfacing for a
    // node when present; they are independent of each other.
    if node {
        if let Some(b) = value.get("blocked-by") {
            if !b.is_null() && !b.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                let _ = writeln!(s, "| blocked-by | {} |", blocked_by_cell(b));
            }
        }
        if let Some(br) = value.get("block-reason") {
            if !br.is_null() {
                let _ = writeln!(s, "| block-reason | {} |", meta_cell(br));
            }
        }
        // The single-holder claim is surfaced only when the node is claimed.
        if let Some(cl) = value.get("claim") {
            if !cl.is_null() {
                let _ = writeln!(s, "| claim | {} |", claim_cell(cl));
            }
        }
    }
    if let Some(cr) = value.get("close-reason") {
        if !cr.is_null() {
            let _ = writeln!(s, "| close-reason | {} |", meta_cell(cr));
        }
    }

    // -- name (H1) and summary (blockquote) ---------------------------------
    let name = value.get("name").and_then(Value::as_str).unwrap_or("");
    let _ = write!(s, "\n# {name}\n");
    let summary = value.get("summary").and_then(Value::as_str).unwrap_or("");
    if !summary.is_empty() {
        let _ = write!(s, "\n> {summary}\n");
    }

    // -- direct-children table ---------------------------------------------
    let child_heading = if node { "Subtickets" } else { "Tickets" };
    let _ = write!(s, "\n## {child_heading}\n");
    if children.is_empty() {
        s.push_str("\n_(none)_\n");
    } else {
        s.push_str("\n| ref | name | status |\n|---|---|---|\n");
        for c in children {
            let _ = writeln!(s, "| {} | {} | {} |", c.reference, c.name, c.status);
        }
    }

    // -- assets table -------------------------------------------------------
    let _ = write!(s, "\n## Assets\n");
    let assets = value
        .get("assets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if assets.is_empty() {
        s.push_str("\n_(none)_\n");
    } else {
        s.push_str("\n| name | description |\n|---|---|\n");
        for a in &assets {
            let name = a.get("name").and_then(Value::as_str).unwrap_or("");
            let desc = a.get("description").and_then(Value::as_str).unwrap_or("");
            let _ = writeln!(s, "| {name} | {desc} |");
        }
    }

    // -- body (verbatim) ----------------------------------------------------
    let _ = write!(s, "\n## Body\n\n");
    let body = value.get("body").and_then(Value::as_str).unwrap_or("");
    s.push_str(body);
    if !body.ends_with('\n') {
        s.push('\n');
    }

    // -- comments -----------------------------------------------------------
    let _ = write!(s, "\n## Comments\n");
    if comment_views.is_empty() {
        s.push_str("\n_(none)_\n");
    } else {
        for c in comment_views {
            s.push('\n');
            s.push_str(&c.to_markdown());
            s.push('\n');
        }
    }

    s
}

/// Render a metadata-cell value: a scalar naturally, an array comma-joined,
/// null as an em dash.
fn meta_cell(v: &Value) -> String {
    match v {
        Value::Null => "—".to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) => "…".to_string(),
    }
}

/// Render a claim cell: the holder identifier and when the claim was taken.
fn claim_cell(c: &Value) -> String {
    let by = c.get("by").and_then(Value::as_str).unwrap_or("");
    let at = c.get("at").and_then(Value::as_str).unwrap_or("");
    format!("{by} (since {at})")
}

/// Render a blocked-by cell: the dependency refs joined by commas.
fn blocked_by_cell(b: &Value) -> String {
    let refs = b
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i.as_str().map(str::to_string))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if refs.is_empty() {
        "—".to_string()
    } else {
        refs
    }
}

/// A comment as the markdown/list renderer sees it: either a full live comment
/// or a tombstone (author + timestamp, text withheld).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentLine {
    /// A live comment shown in full.
    Live {
        /// The comment id.
        id: u64,
        /// The author in output form (`human` / `agent:<name>`).
        author: String,
        /// The creation timestamp.
        created: String,
        /// The full text.
        text: String,
    },
    /// A soft-deleted comment shown as a tombstone: its text is withheld.
    Tombstone {
        /// The comment id.
        id: u64,
        /// The author in output form.
        author: String,
        /// The creation timestamp.
        created: String,
    },
}

impl CommentLine {
    /// Render this comment for the markdown `show` comments section.
    fn to_markdown(&self) -> String {
        match self {
            CommentLine::Live {
                id,
                author,
                created,
                text,
            } => {
                let mut s = format!("**#{id}** {author} · {created}\n");
                s.push('\n');
                s.push_str(text);
                if !text.ends_with('\n') {
                    s.push('\n');
                }
                s
            }
            CommentLine::Tombstone {
                id,
                author,
                created,
            } => {
                format!("**#{id}** {author} · {created} · _(deleted)_\n")
            }
        }
    }
}

// ===========================================================================
// list
// ===========================================================================

/// One node in a list result, already located by its reference and parent. The
/// list renderers work over a slice of these, which the caller resolves from
/// the store (scope + filters live in a later layer).
#[derive(Debug, Clone)]
pub struct ListNode {
    /// The node's `<epic-id>/<n>` reference.
    pub reference: String,
    /// Its number within the epic.
    pub number: u64,
    /// Its one-line name.
    pub name: String,
    /// Its state's wire name.
    pub status: String,
    /// Its parent's reference, if any (the flat forms carry this pointer).
    pub parent: Option<String>,
    /// Its labels.
    pub labels: Vec<String>,
    /// The node's `blocked-by` dependency refs (canonical `<epic-id>/<n>`).
    /// Rendered as a trailing `[blocked-by: …]` tag; independent of `status`.
    pub blocked_by: Vec<String>,
}

/// A list roster entry for an epic (the `epic list` roster).
#[derive(Debug, Clone)]
pub struct ListEpic {
    /// The epic id.
    pub id: String,
    /// Its one-line name.
    pub name: String,
    /// Its computed status's wire name.
    pub status: String,
    /// Its labels.
    pub labels: Vec<String>,
    /// How many nodes it holds.
    pub nodes: usize,
}

/// Optional ANSI styling for the plain-text list. Callers that write to a TTY
/// pass `Color::Ansi`; piped output passes `Color::None`. The machine forms
/// (json/ndjson/raw) never receive colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// No escape codes at all.
    None,
    /// Emit ANSI styling.
    Ansi,
}

/// Render the default plain-text `list` for nodes: a git-log-like, indented,
/// depth-first tree, each node with dependencies carrying a trailing
/// `[blocked-by: …]` tag.
///
/// The tree is reconstructed from each node's parent pointer; a node whose
/// parent is not in the set (e.g. because the scope started below it) is treated
/// as a root of the shown forest, so the output is well-formed for any scope.
pub fn list_nodes_plain(nodes: &[ListNode], color: Color) -> String {
    // Index children by parent reference; roots are those whose parent is absent
    // or points outside the shown set.
    let present: std::collections::HashSet<&str> =
        nodes.iter().map(|n| n.reference.as_str()).collect();
    let mut children: std::collections::HashMap<Option<&str>, Vec<&ListNode>> =
        std::collections::HashMap::new();
    for n in nodes {
        let key = match &n.parent {
            Some(p) if present.contains(p.as_str()) => Some(p.as_str()),
            _ => None,
        };
        children.entry(key).or_default().push(n);
    }
    // Stable order within each sibling group: ascending node number.
    for group in children.values_mut() {
        group.sort_by_key(|n| n.number);
    }

    let mut out = String::new();
    let mut stack: Vec<(&ListNode, usize)> = Vec::new();
    // Seed with roots in ascending order, pushed reversed so the stack pops
    // them ascending (depth-first, children before the next sibling).
    if let Some(roots) = children.get(&None) {
        for root in roots.iter().rev() {
            stack.push((root, 0));
        }
    }
    while let Some((node, depth)) = stack.pop() {
        out.push_str(&render_node_line(node, depth, color));
        out.push('\n');
        if let Some(kids) = children.get(&Some(node.reference.as_str())) {
            for kid in kids.iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
    }
    out
}

/// One indented node line: `<indent><ref> <name> <status>` plus a trailing
/// `[blocked-by: …]` tag when the node has dependencies. Colour, when enabled,
/// dims the reference and paints the status; the dependency tag is emphasised.
fn render_node_line(node: &ListNode, depth: usize, color: Color) -> String {
    let indent = "  ".repeat(depth);
    let reference = paint(&node.reference, dim(), color);
    let status = paint(
        &format!("({})", node.status),
        status_style(&node.status),
        color,
    );
    let mut line = format!("{indent}{reference} {} {status}", node.name);
    if !node.blocked_by.is_empty() {
        let tag_text = format!("[blocked-by: {}]", node.blocked_by.join(", "));
        let _ = write!(line, " {}", paint(&tag_text, blocked_style(), color));
    }
    line
}

/// Node statuses in lifecycle order — the order the progress summary lists them.
const STATUS_LIFECYCLE: &[&str] = &["to-do", "in-progress", "blocked", "done", "closed"];

/// The per-status progress summary that closes the plain `ticket list`: a total
/// plus one entry per non-empty status in lifecycle order (e.g.
/// `8 tickets · 2 to-do · 1 in-progress · 1 blocked · 3 done · 1 closed`).
///
/// The count is always over the nodes actually listed — scope, `--shallow` and
/// filters are already applied — so `filtered` tags the line, keeping a narrowed
/// count from being read as the whole scope. An all-terminal set is marked done
/// with a trailing check. Plain-text only: machine formats stay pure data and
/// never carry this line.
pub fn list_summary(nodes: &[ListNode], filtered: bool, color: Color) -> String {
    let total = nodes.len();
    let counts: Vec<(usize, &str)> = STATUS_LIFECYCLE
        .iter()
        .filter_map(|st| {
            let c = nodes.iter().filter(|n| n.status == *st).count();
            (c > 0).then_some((c, *st))
        })
        .collect();
    // Terminal = done|closed; an all-terminal, non-empty set is "finished".
    let all_terminal = total > 0
        && nodes
            .iter()
            .all(|n| n.status == "done" || n.status == "closed");

    let line = summary_line(total, &counts, all_terminal, filtered, color);
    if total == 0 {
        // Nothing to rule off against — the count stands on its own.
        return format!("{line}\n");
    }
    // The divider spans the widest visible line so the summary reads as a footer;
    // measured uncolored (ANSI has no width) and capped so it stays sane.
    let plain_line = summary_line(total, &counts, all_terminal, filtered, Color::None);
    let width = list_nodes_plain(nodes, Color::None)
        .lines()
        .map(|l| l.chars().count())
        .chain(std::iter::once(plain_line.chars().count()))
        .max()
        .unwrap_or(0)
        .clamp(1, 72);
    let divider = paint(&"\u{2500}".repeat(width), dim(), color);
    format!("{divider}\n{line}\n")
}

/// Build the one-line summary, painting each status count in its status colour
/// when colour is enabled.
fn summary_line(
    total: usize,
    counts: &[(usize, &str)],
    all_terminal: bool,
    filtered: bool,
    color: Color,
) -> String {
    let ticket_word = if total == 1 { "ticket" } else { "tickets" };
    let mut line = format!("{total} {ticket_word}");
    for (c, st) in counts {
        let _ = write!(
            line,
            " · {}",
            paint(&format!("{c} {st}"), status_style(st), color)
        );
    }
    if all_terminal {
        let _ = write!(line, "  {}", paint("✓", status_style("done"), color));
    }
    if filtered {
        let _ = write!(line, "  {}", paint("(filtered)", dim(), color));
    }
    line
}

/// Render the plain-text `epic list` roster: one line per epic with its status,
/// node count and labels.
pub fn list_epics_plain(epics: &[ListEpic], color: Color) -> String {
    let mut out = String::new();
    for e in epics {
        let id = paint(&e.id, dim(), color);
        let status = paint(
            &format!("({})", e.status),
            epic_status_style(&e.status),
            color,
        );
        let labels = if e.labels.is_empty() {
            String::new()
        } else {
            format!(" [{}]", e.labels.join(", "))
        };
        let _ = writeln!(
            out,
            "{id} {} {status} — {} node(s){labels}",
            e.name, e.nodes
        );
    }
    out
}

/// Render `list --json` for nodes: a flat array of objects, each carrying its
/// `parent` pointer. Never nested — hierarchy is reconstructable from the
/// pointers, per the flat result model.
pub fn list_nodes_json(nodes: &[ListNode]) -> String {
    to_json_string(&Value::Array(nodes.iter().map(list_node_value).collect()))
}

/// Render `list --ndjson` for nodes: one flat JSON object per line.
pub fn list_nodes_ndjson(nodes: &[ListNode]) -> String {
    let mut out = String::new();
    for n in nodes {
        out.push_str(&to_json_line(&list_node_value(n)));
        out.push('\n');
    }
    out
}

/// Render `list --raw` for nodes: flat, tab-separated rows
/// (`ref  number  name  status  parent  labels  blocked-by`). The blocked-by
/// column joins the dependency refs with commas.
pub fn list_nodes_raw(nodes: &[ListNode]) -> String {
    let mut out = String::new();
    for n in nodes {
        let parent = n.parent.clone().unwrap_or_default();
        let labels = n.labels.join(",");
        let blocked_by = n.blocked_by.join(",");
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            n.reference, n.number, n.name, n.status, parent, labels, blocked_by
        );
    }
    out
}

fn list_node_value(n: &ListNode) -> Value {
    json!({
        "ref": n.reference,
        "number": n.number,
        "name": n.name,
        "status": n.status,
        "parent": n.parent.clone().map(Value::from).unwrap_or(Value::Null),
        "labels": n.labels,
        "blocked-by": n.blocked_by,
    })
}

/// Render `epic list --json`: a flat array of epic roster objects.
pub fn list_epics_json(epics: &[ListEpic]) -> String {
    to_json_string(&Value::Array(epics.iter().map(list_epic_value).collect()))
}

/// Render `epic list --ndjson`: one epic roster object per line.
pub fn list_epics_ndjson(epics: &[ListEpic]) -> String {
    let mut out = String::new();
    for e in epics {
        out.push_str(&to_json_line(&list_epic_value(e)));
        out.push('\n');
    }
    out
}

/// Render `epic list --raw`: tab-separated `id  name  status  labels  nodes`.
pub fn list_epics_raw(epics: &[ListEpic]) -> String {
    let mut out = String::new();
    for e in epics {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            e.id,
            e.name,
            e.status,
            e.labels.join(","),
            e.nodes
        );
    }
    out
}

fn list_epic_value(e: &ListEpic) -> Value {
    json!({
        "id": e.id,
        "name": e.name,
        "status": e.status,
        "labels": e.labels,
        "nodes": e.nodes,
    })
}

/// The node fields `list` may serve via `--fields`. Requesting anything else is
/// a hard error — heavy/structured fields are `show`-only.
pub const LISTABLE_NODE_FIELDS: &[&str] = &[
    "ref",
    "number",
    "name",
    "status",
    "parent",
    "labels",
    "blocked-by",
];

/// The epic roster fields `list` may serve via `--fields`.
pub const LISTABLE_EPIC_FIELDS: &[&str] = &["id", "name", "status", "labels", "nodes"];

/// The heavy/structured fields that are `show`-only and rejected on `list`.
pub const HEAVY_FIELDS: &[&str] = &["body", "comments", "assets", "subtickets", "claim"];

/// Validate that every requested `list` field is a listable summary field. A
/// heavy/structured field, or any unknown field, is rejected so `list` never
/// silently serves show-only data.
pub fn validate_list_fields(fields: &[String], listable: &[&str]) -> Result<(), RenderError> {
    for f in fields {
        // Only the head segment is checked against the listable set; a dotted
        // path into a listable field is still summary.
        let head = f.split('.').next().unwrap_or(f);
        if listable.contains(&head) {
            continue;
        }
        if HEAVY_FIELDS.contains(&head) {
            return Err(RenderError::FieldNotListable {
                field: f.clone(),
                listable: format!("list serves {}", listable.join("|")),
            });
        }
        return Err(RenderError::UnknownField(f.clone()));
    }
    Ok(())
}

/// Render a `list --raw`/tab projection over listable node fields: one
/// tab-separated row per node, columns in the requested field order.
pub fn list_nodes_fields_raw(nodes: &[ListNode], fields: &[String]) -> String {
    let mut out = String::new();
    for n in nodes {
        let value = list_node_value(n);
        let cols: Vec<String> = fields
            .iter()
            .map(|f| {
                let vals = project_path(&value, f);
                vals.iter()
                    .map(|v| leaf_to_raw(v).unwrap_or_else(|| to_json_line(v)))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect();
        let _ = writeln!(out, "{}", cols.join("\t"));
    }
    out
}

// ===========================================================================
// colour helpers (used only by the plain-text list)
// ===========================================================================

fn paint(text: &str, style: anstyle::Style, color: Color) -> String {
    match color {
        Color::None => text.to_string(),
        Color::Ansi => format!("{}{}{}", style.render(), text, style.render_reset()),
    }
}

fn dim() -> anstyle::Style {
    anstyle::Style::new().dimmed()
}

fn blocked_style() -> anstyle::Style {
    anstyle::Style::new()
        .fg_color(Some(anstyle::AnsiColor::Yellow.into()))
        .bold()
}

fn status_style(status: &str) -> anstyle::Style {
    hue_style(node_status_hue(status))
}

fn epic_status_style(state: &str) -> anstyle::Style {
    hue_style(epic_status_hue(state))
}

fn hue_style(hue: Hue) -> anstyle::Style {
    let color = match hue {
        Hue::Resolved => anstyle::AnsiColor::Green,
        Hue::Abandoned => anstyle::AnsiColor::BrightBlack,
        Hue::Attention => anstyle::AnsiColor::Yellow,
        Hue::Active => anstyle::AnsiColor::Cyan,
        Hue::Pending => anstyle::AnsiColor::White,
    };
    anstyle::Style::new().fg_color(Some(color.into()))
}

/// The terminal-neutral hue a status is painted in — the single definition of
/// loti's status palette, shared by every surface. Each surface maps a hue to
/// its own colour type (ANSI for the plain text forms, a widget colour for a
/// full-screen UI) so no surface restates which status is which colour and two
/// views of the same store can never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hue {
    /// Work not started.
    Pending,
    /// Work under way.
    Active,
    /// Work that needs a human's attention before it can move.
    Attention,
    /// Work delivered successfully.
    Resolved,
    /// Work resolved without being completed.
    Abandoned,
}

/// The hue for a node status in its wire form (`to-do`, `in-progress`,
/// `blocked`, `done`, `closed`). An unknown status reads as pending rather than
/// failing: a palette lookup never gates a read.
pub fn node_status_hue(status: &str) -> Hue {
    match status {
        "done" => Hue::Resolved,
        "closed" => Hue::Abandoned,
        "blocked" => Hue::Attention,
        "in-progress" => Hue::Active,
        _ => Hue::Pending,
    }
}

/// The hue for an epic state in its wire form (`open`, `completed`, `closed`).
/// An epic is never "pending": an epic with no tickets is open, which is the
/// active hue.
pub fn epic_status_hue(state: &str) -> Hue {
    match state {
        "completed" => Hue::Resolved,
        "closed" => Hue::Abandoned,
        _ => Hue::Active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "ref": "e/1",
            "number": 1,
            "name": "do a thing",
            "summary": "a slice",
            "status": "in-progress",
            "labels": ["a", "b"],
            "parent": Value::Null,
            "assets": [{"name": "x.png", "description": Value::Null}],
            "comments": [
                {"id": 1, "author": "human", "created": "t", "text": "hi", "deleted": false},
                {"id": 2, "author": "agent:bot", "created": "t", "text": "yo", "deleted": false},
            ],
            "body": "the body\n",
        })
    }

    // -- projector ---------------------------------------------------------

    #[test]
    fn project_single_scalar_leaf() {
        assert_eq!(
            project_path(&sample(), "status"),
            vec![json!("in-progress")]
        );
    }

    #[test]
    fn project_distributes_over_a_repeated_field() {
        // `comments.author` yields one author per comment, in order.
        assert_eq!(
            project_path(&sample(), "comments.author"),
            vec![json!("human"), json!("agent:bot")]
        );
    }

    #[test]
    fn project_unknown_path_is_empty() {
        assert!(project_path(&sample(), "nope").is_empty());
    }

    // -- show --raw --------------------------------------------------------

    #[test]
    fn raw_single_leaf_renders_unquoted() {
        let out = show_raw(&sample(), &Projection::One("status".into())).unwrap();
        assert_eq!(out, "in-progress");
    }

    #[test]
    fn raw_distributed_leaves_one_per_line() {
        let out = show_raw(&sample(), &Projection::One("comments.author".into())).unwrap();
        assert_eq!(out, "human\nagent:bot");
    }

    #[test]
    fn raw_whole_structured_field_is_ambiguous() {
        // Selecting a whole repeated/structured field has no one-per-line form.
        let err = show_raw(&sample(), &Projection::One("assets".into())).unwrap_err();
        assert!(matches!(err, RenderError::RawAmbiguous { .. }));
    }

    #[test]
    fn raw_whole_value_without_projection_is_ambiguous() {
        let err = show_raw(&sample(), &Projection::Whole).unwrap_err();
        assert!(matches!(err, RenderError::RawAmbiguous { .. }));
        // But a value that is itself a single scalar is fine.
        assert_eq!(
            show_raw(&json!("leaf"), &Projection::Whole).unwrap(),
            "leaf"
        );
    }

    // -- show --json -------------------------------------------------------

    #[test]
    fn json_projection_single_and_many() {
        let one = show_json(&sample(), &Projection::One("status".into())).unwrap();
        assert_eq!(one.trim(), "\"in-progress\"");
        let many = show_json(
            &sample(),
            &Projection::Many(vec!["status".into(), "number".into()]),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&many).unwrap();
        assert_eq!(v["status"], "in-progress");
        assert_eq!(v["number"], 1);
    }

    #[test]
    fn json_unknown_projection_errors() {
        assert!(matches!(
            show_json(&sample(), &Projection::One("ghost".into())),
            Err(RenderError::UnknownField(_))
        ));
    }

    // -- markdown ----------------------------------------------------------

    #[test]
    fn markdown_orders_all_sections() {
        let children = vec![ChildRow {
            reference: "e/2".into(),
            name: "child".into(),
            status: "to-do".into(),
        }];
        let comments = vec![CommentLine::Live {
            id: 1,
            author: "human".into(),
            created: "t".into(),
            text: "a note".into(),
        }];
        let md = show_markdown(&sample(), &children, &comments);
        let order = [
            "| field | value |",
            "\n# do a thing",
            "> a slice",
            "## Subtickets",
            "## Assets",
            "## Body",
            "## Comments",
        ];
        let mut last = 0;
        for marker in order {
            let at = md
                .find(marker)
                .unwrap_or_else(|| panic!("missing {marker}"));
            assert!(at >= last, "section out of order: {marker}");
            last = at;
        }
        assert!(md.contains("e/2"));
        assert!(md.contains("the body"));
    }

    // -- list --------------------------------------------------------------

    fn nodes() -> Vec<ListNode> {
        vec![
            ListNode {
                reference: "e/1".into(),
                number: 1,
                name: "root".into(),
                status: "in-progress".into(),
                parent: None,
                labels: vec!["x".into()],
                blocked_by: vec![],
            },
            ListNode {
                reference: "e/2".into(),
                number: 2,
                name: "child".into(),
                status: "blocked".into(),
                parent: Some("e/1".into()),
                labels: vec![],
                blocked_by: vec!["e/9".into(), "e/12".into()],
            },
        ]
    }

    #[test]
    fn plain_list_is_depth_first_indented_with_blocked_by_tag() {
        let out = list_nodes_plain(&nodes(), Color::None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "e/1 root (in-progress)");
        assert!(lines[1].starts_with("  e/2 "));
        // The tag lists the dependency refs, independent of status.
        assert!(lines[1].contains("[blocked-by: e/9, e/12]"));
    }

    fn node(reference: &str, number: u64, status: &str) -> ListNode {
        ListNode {
            reference: reference.into(),
            number,
            name: "n".into(),
            status: status.into(),
            parent: None,
            labels: vec![],
            blocked_by: vec![],
        }
    }

    #[test]
    fn summary_counts_each_status_in_lifecycle_order() {
        let ns = vec![
            node("e/1", 1, "done"),
            node("e/2", 2, "to-do"),
            node("e/3", 3, "blocked"),
            node("e/4", 4, "done"),
        ];
        let s = list_summary(&ns, false, Color::None);
        let line = s.lines().last().unwrap();
        // Total, then only non-empty statuses, in lifecycle order (to-do before
        // blocked before done); no in-progress or closed entry appears.
        assert_eq!(line, "4 tickets · 1 to-do · 1 blocked · 2 done");
    }

    #[test]
    fn summary_marks_an_all_terminal_set_done_and_tags_a_filtered_one() {
        let done = vec![node("e/1", 1, "done"), node("e/2", 2, "closed")];
        assert!(
            list_summary(&done, false, Color::None)
                .lines()
                .last()
                .unwrap()
                .ends_with("✓"),
            "all-terminal set is marked finished"
        );
        let filtered = list_summary(&[node("e/1", 1, "to-do")], true, Color::None);
        assert!(filtered.contains("(filtered)"), "narrowed count is tagged");
    }

    #[test]
    fn summary_of_an_empty_set_is_a_bare_count_with_no_divider() {
        let s = list_summary(&[], false, Color::None);
        assert_eq!(s, "0 tickets\n");
    }

    #[test]
    fn plain_list_colour_only_when_asked() {
        let plain = list_nodes_plain(&nodes(), Color::None);
        let ansi = list_nodes_plain(&nodes(), Color::Ansi);
        // The escape byte appears only under Color::Ansi.
        assert!(!plain.contains('\u{1b}'));
        assert!(ansi.contains('\u{1b}'));
    }

    #[test]
    fn list_json_is_flat_with_parent_pointers_never_nested() {
        let out = list_nodes_json(&nodes());
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1]["parent"], "e/1");
        assert!(arr[0].get("children").is_none());
    }

    #[test]
    fn list_ndjson_one_object_per_line() {
        let out = list_nodes_ndjson(&nodes());
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        for l in lines {
            let _: Value = serde_json::from_str(l).unwrap();
        }
    }

    #[test]
    fn list_raw_is_tab_separated() {
        let out = list_nodes_raw(&nodes());
        let first = out.lines().next().unwrap();
        let cols: Vec<&str> = first.split('\t').collect();
        // ref, number, name, status, parent, labels, blocked-by
        assert_eq!(cols.len(), 7);
        assert_eq!(cols[0], "e/1");
    }

    // -- listable-field gate ----------------------------------------------

    #[test]
    fn listable_fields_pass_heavy_fields_fail() {
        assert!(validate_list_fields(
            &["ref".into(), "status".into(), "blocked-by".into()],
            LISTABLE_NODE_FIELDS
        )
        .is_ok());
        assert!(matches!(
            validate_list_fields(&["body".into()], LISTABLE_NODE_FIELDS),
            Err(RenderError::FieldNotListable { .. })
        ));
        assert!(matches!(
            validate_list_fields(&["comments".into()], LISTABLE_NODE_FIELDS),
            Err(RenderError::FieldNotListable { .. })
        ));
        assert!(matches!(
            validate_list_fields(&["nope".into()], LISTABLE_NODE_FIELDS),
            Err(RenderError::UnknownField(_))
        ));
    }

    #[test]
    fn epic_json_state_is_computed() {
        use crate::domain::NodeStatus;
        use crate::model::{EpicFile, EpicFrontmatter};
        use crate::NodeState;
        let epic = EpicFile {
            frontmatter: EpicFrontmatter {
                id: "e".into(),
                name: "n".into(),
                summary: "s".into(),
                next_number: 3,
                closed: false,
                close_reason: None,
                labels: vec![],
                assets: vec![],
                comments: vec![],
                created: "2024-01-01T00:00:00Z".parse().unwrap(),
                updated: "2024-01-01T00:00:00Z".parse().unwrap(),
                extra: serde_yaml::Mapping::new(),
            },
            body: String::new(),
        };
        // All nodes terminal => completed.
        let done = [
            NodeStatus::new(1, NodeState::Done),
            NodeStatus::new(2, NodeState::Closed),
        ];
        let v = epic_to_json(&epic, &done);
        assert_eq!(v["status"], "completed");
        assert_eq!(v["nodes"], 2);
        // No nodes => open.
        let v0 = epic_to_json(&epic, &[]);
        assert_eq!(v0["status"], "open");
    }

    #[test]
    fn unknown_frontmatter_keys_survive_into_json() {
        use crate::model::NodeFile;
        let text = "---\n\
             number: 1\n\
             name: n\n\
             summary: s\n\
             status: to-do\n\
             created: 2024-01-01T00:00:00Z\n\
             updated: 2024-01-01T00:00:00Z\n\
             future-key: kept\n\
             ---\nbody\n";
        let node = NodeFile::parse(text).unwrap();
        let v = node_to_json("e", &node);
        assert_eq!(v["future-key"], "kept");
    }
}
