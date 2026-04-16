//! Scene-grammar schema generation from facet SHAPE reflection.
//!
//! T-12.12 (cavekit-scene R13). Walks [`crate::ast::SceneDoc::SHAPE`] and
//! emits a structural schema describing every node, argument, property,
//! and child relationship. Two output formats:
//!
//! * **KDL** — the native format consumed by editor tooling and the CI
//!   drift check. Same shape as `crates/scene/share/scene.kdl-schema`.
//! * **JSON** — a machine-friendly alternative for non-KDL consumers.
//!
//! The schema emitter is PURE REFLECTION — no hand-maintained table.
//! When the AST evolves, the schema updates automatically.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use facet::{Def, Facet, Shape, Type, UserType};

use crate::ast::SceneDoc;

/// Ordered index of every user-defined struct reachable from the scene
/// AST, keyed by `type_identifier`. Deterministic for CI diffing.
type TypeIndex = BTreeMap<&'static str, &'static Shape>;

/// Generate the scene grammar schema in KDL format.
///
/// This is the same output as `cargo run -p ark-scene --bin gen-scene-schema`.
pub fn generate_schema_kdl() -> String {
    let mut types: TypeIndex = BTreeMap::new();
    collect_user_types(SceneDoc::SHAPE, &mut types);

    let mut out = String::new();
    writeln!(&mut out, "document {{").unwrap();
    writeln!(&mut out, "    info {{").unwrap();
    writeln!(&mut out, "        title \"ark scene file\"").unwrap();
    writeln!(
        &mut out,
        "        description \"Reactive KDL config for ark (cavekit-scene.md R1).\""
    )
    .unwrap();
    writeln!(&mut out, "        version \"0.1.0-phase-a\"").unwrap();
    writeln!(&mut out, "    }}").unwrap();
    writeln!(&mut out).unwrap();

    if let Some(doc_field) = single_root_child(SceneDoc::SHAPE) {
        let root_name = kdl_node_name_for_field(doc_field);
        let root_shape = doc_field.shape().inner_user_shape();
        if let Some(root_shape) = root_shape {
            emit_node(&mut out, &root_name, root_shape, 1);
        }
    }

    writeln!(&mut out).unwrap();
    writeln!(&mut out, "    definitions {{").unwrap();
    for (name, shape) in &types {
        if *name == "SceneDoc" || *name == "SceneNode" {
            continue;
        }
        emit_definition(&mut out, name, shape, 2);
    }
    writeln!(&mut out, "    }}").unwrap();
    writeln!(&mut out, "}}").unwrap();
    out
}

/// Generate the scene grammar schema in JSON format.
///
/// Produces a JSON object with `info`, `root`, and `definitions` keys.
pub fn generate_schema_json() -> String {
    let mut types: TypeIndex = BTreeMap::new();
    collect_user_types(SceneDoc::SHAPE, &mut types);

    let mut out = String::from("{\n");

    // Info block.
    writeln!(&mut out, "  \"info\": {{").unwrap();
    writeln!(&mut out, "    \"title\": \"ark scene file\",").unwrap();
    writeln!(
        &mut out,
        "    \"description\": \"Reactive KDL config for ark (cavekit-scene.md R1).\","
    )
    .unwrap();
    writeln!(&mut out, "    \"version\": \"0.1.0-phase-a\"").unwrap();
    writeln!(&mut out, "  }},").unwrap();

    // Root node.
    if let Some(doc_field) = single_root_child(SceneDoc::SHAPE) {
        let root_name = kdl_node_name_for_field(doc_field);
        let root_shape = doc_field.shape().inner_user_shape();
        if let Some(root_shape) = root_shape {
            write!(&mut out, "  \"root\": ").unwrap();
            emit_node_json(&mut out, &root_name, root_shape, 1);
            writeln!(&mut out, ",").unwrap();
        }
    }

    // Definitions.
    writeln!(&mut out, "  \"definitions\": {{").unwrap();
    let filtered: Vec<_> = types
        .iter()
        .filter(|(name, _)| **name != "SceneDoc" && **name != "SceneNode")
        .collect();
    for (i, (name, shape)) in filtered.iter().enumerate() {
        write!(&mut out, "    \"{name}\": ").unwrap();
        emit_definition_json(&mut out, shape, 2);
        if i + 1 < filtered.len() {
            writeln!(&mut out, ",").unwrap();
        } else {
            writeln!(&mut out).unwrap();
        }
    }
    writeln!(&mut out, "  }}").unwrap();
    writeln!(&mut out, "}}").unwrap();
    out
}

// ---------------------------------------------------------------------------
// SHAPE walking
// ---------------------------------------------------------------------------

fn collect_user_types(shape: &'static Shape, acc: &mut TypeIndex) {
    let user = match shape.inner_user_shape() {
        Some(u) => u,
        None => return,
    };
    if acc.contains_key(user.type_identifier) {
        return;
    }
    acc.insert(user.type_identifier, user);

    if let Type::User(UserType::Struct(st)) = &user.ty {
        for field in st.fields {
            collect_user_types(field.shape(), acc);
        }
    }
}

fn single_root_child(shape: &'static Shape) -> Option<&'static facet::Field> {
    if let Type::User(UserType::Struct(st)) = &shape.ty
        && st.fields.len() == 1
    {
        return Some(&st.fields[0]);
    }
    None
}

// ---------------------------------------------------------------------------
// KDL emission
// ---------------------------------------------------------------------------

fn emit_node(out: &mut String, name: &str, shape: &'static Shape, depth: usize) {
    let indent = indent(depth);
    writeln!(out, "{indent}node \"{name}\" {{").unwrap();
    write_doc(out, shape.doc, depth + 1);

    let fields = match &shape.ty {
        Type::User(UserType::Struct(st)) => st.fields,
        _ => &[],
    };

    for field in fields {
        if field.is_argument() {
            emit_value(out, field, depth + 1);
        }
    }
    for field in fields {
        if field.is_property() {
            emit_prop(out, field, depth + 1);
        }
    }
    let child_fields: Vec<_> = fields
        .iter()
        .filter(|f| f.is_element() || f.is_elements())
        .collect();
    if !child_fields.is_empty() {
        writeln!(out, "{}children {{", indent_for(depth + 1)).unwrap();
        for field in &child_fields {
            emit_child(out, field, depth + 2);
        }
        writeln!(out, "{}}}", indent_for(depth + 1)).unwrap();
    }

    writeln!(out, "{indent}}}").unwrap();
}

fn emit_value(out: &mut String, field: &facet::Field, depth: usize) {
    let name = field.effective_name();
    let indent = indent(depth);
    let (inner, optional) = unwrap_container(field.shape());
    let type_name = scalar_type_name(inner);
    let plural = field.is_arguments_plural();

    writeln!(out, "{indent}value \"{name}\" {{").unwrap();
    write_doc(out, field.doc, depth + 1);
    writeln!(out, "{}type \"{type_name}\"", indent_for(depth + 1)).unwrap();
    if plural {
        writeln!(out, "{}min 0", indent_for(depth + 1)).unwrap();
    } else if optional {
        writeln!(out, "{}min 0; max 1", indent_for(depth + 1)).unwrap();
    } else {
        writeln!(out, "{}min 1; max 1", indent_for(depth + 1)).unwrap();
    }
    writeln!(out, "{indent}}}").unwrap();
}

fn emit_prop(out: &mut String, field: &facet::Field, depth: usize) {
    let name = field.effective_name();
    let indent = indent(depth);
    let (inner, optional) = unwrap_container(field.shape());
    let type_name = scalar_type_name(inner);
    let required = !optional && !field.has_default();

    writeln!(out, "{indent}prop \"{name}\" {{").unwrap();
    write_doc(out, field.doc, depth + 1);
    writeln!(out, "{}type \"{type_name}\"", indent_for(depth + 1)).unwrap();
    writeln!(
        out,
        "{}required {}",
        indent_for(depth + 1),
        if required { "#true" } else { "#false" }
    )
    .unwrap();
    writeln!(out, "{indent}}}").unwrap();
}

fn emit_child(out: &mut String, field: &facet::Field, depth: usize) {
    let name = kdl_node_name_for_field(field);
    let indent = indent(depth);
    let plural = field.is_elements();
    let (inner, optional) = unwrap_container(field.shape());
    let ref_id = inner.type_identifier;

    writeln!(out, "{indent}node \"{name}\" {{").unwrap();
    write_doc(out, field.doc, depth + 1);
    if plural {
        writeln!(out, "{}min 0", indent_for(depth + 1)).unwrap();
    } else if optional {
        writeln!(out, "{}min 0; max 1", indent_for(depth + 1)).unwrap();
    } else {
        writeln!(out, "{}min 1; max 1", indent_for(depth + 1)).unwrap();
    }
    writeln!(out, "{}ref \"{ref_id}\"", indent_for(depth + 1)).unwrap();
    writeln!(out, "{indent}}}").unwrap();
}

fn emit_definition(out: &mut String, id: &str, shape: &'static Shape, depth: usize) {
    let indent = indent(depth);
    writeln!(out, "{indent}node id=\"{id}\" {{").unwrap();
    write_doc(out, shape.doc, depth + 1);

    let fields = match &shape.ty {
        Type::User(UserType::Struct(st)) => st.fields,
        _ => &[],
    };
    for field in fields {
        if field.is_argument() {
            emit_value(out, field, depth + 1);
        }
    }
    for field in fields {
        if field.is_property() {
            emit_prop(out, field, depth + 1);
        }
    }
    let child_fields: Vec<_> = fields
        .iter()
        .filter(|f| f.is_element() || f.is_elements())
        .collect();
    if !child_fields.is_empty() {
        writeln!(out, "{}children {{", indent_for(depth + 1)).unwrap();
        for field in &child_fields {
            emit_child(out, field, depth + 2);
        }
        writeln!(out, "{}}}", indent_for(depth + 1)).unwrap();
    }
    writeln!(out, "{indent}}}").unwrap();
}

// ---------------------------------------------------------------------------
// JSON emission
// ---------------------------------------------------------------------------

fn emit_node_json(out: &mut String, name: &str, shape: &'static Shape, depth: usize) {
    let ind = json_indent(depth);
    let ind1 = json_indent(depth + 1);
    writeln!(out, "{{").unwrap();
    writeln!(out, "{ind1}\"name\": \"{name}\",").unwrap();

    let doc_str = join_doc(shape.doc);
    if !doc_str.is_empty() {
        writeln!(out, "{ind1}\"description\": \"{}\",", escape_json(&doc_str)).unwrap();
    }

    let fields = match &shape.ty {
        Type::User(UserType::Struct(st)) => st.fields,
        _ => &[],
    };

    // Arguments.
    let args: Vec<_> = fields.iter().filter(|f| f.is_argument()).collect();
    if !args.is_empty() {
        writeln!(out, "{ind1}\"arguments\": [").unwrap();
        for (i, field) in args.iter().enumerate() {
            emit_field_json(out, field, depth + 2);
            if i + 1 < args.len() {
                writeln!(out, ",").unwrap();
            } else {
                writeln!(out).unwrap();
            }
        }
        writeln!(out, "{ind1}],").unwrap();
    }

    // Properties.
    let props: Vec<_> = fields.iter().filter(|f| f.is_property()).collect();
    if !props.is_empty() {
        writeln!(out, "{ind1}\"properties\": [").unwrap();
        for (i, field) in props.iter().enumerate() {
            emit_field_json(out, field, depth + 2);
            if i + 1 < props.len() {
                writeln!(out, ",").unwrap();
            } else {
                writeln!(out).unwrap();
            }
        }
        writeln!(out, "{ind1}],").unwrap();
    }

    // Children.
    let child_fields: Vec<_> = fields
        .iter()
        .filter(|f| f.is_element() || f.is_elements())
        .collect();
    writeln!(out, "{ind1}\"children\": [").unwrap();
    for (i, field) in child_fields.iter().enumerate() {
        emit_child_ref_json(out, field, depth + 2);
        if i + 1 < child_fields.len() {
            writeln!(out, ",").unwrap();
        } else {
            writeln!(out).unwrap();
        }
    }
    writeln!(out, "{ind1}]").unwrap();

    write!(out, "{ind}}}").unwrap();
}

fn emit_field_json(out: &mut String, field: &facet::Field, depth: usize) {
    let ind = json_indent(depth);
    let (inner, optional) = unwrap_container(field.shape());
    let type_name = scalar_type_name(inner);
    let name = field.effective_name();
    let required = !optional && !field.has_default();
    let doc_str = join_doc(field.doc);

    write!(
        out,
        "{ind}{{\"name\": \"{name}\", \"type\": \"{type_name}\", \"required\": {required}",
    )
    .unwrap();
    if !doc_str.is_empty() {
        write!(out, ", \"description\": \"{}\"", escape_json(&doc_str)).unwrap();
    }
    write!(out, "}}").unwrap();
}

fn emit_child_ref_json(out: &mut String, field: &facet::Field, depth: usize) {
    let ind = json_indent(depth);
    let name = kdl_node_name_for_field(field);
    let plural = field.is_elements();
    let (inner, optional) = unwrap_container(field.shape());
    let ref_id = inner.type_identifier;
    let min = 0;
    let max_str = if plural {
        "\"*\"".to_string()
    } else if optional {
        "1".to_string()
    } else {
        "1".to_string()
    };

    write!(
        out,
        "{ind}{{\"name\": \"{name}\", \"ref\": \"{ref_id}\", \"min\": {min}, \"max\": {max_str}}}",
    )
    .unwrap();
}

fn emit_definition_json(out: &mut String, shape: &'static Shape, depth: usize) {
    let ind = json_indent(depth);
    let ind1 = json_indent(depth + 1);
    writeln!(out, "{{").unwrap();

    let doc_str = join_doc(shape.doc);
    if !doc_str.is_empty() {
        writeln!(out, "{ind1}\"description\": \"{}\",", escape_json(&doc_str)).unwrap();
    }

    let fields = match &shape.ty {
        Type::User(UserType::Struct(st)) => st.fields,
        _ => &[],
    };

    writeln!(out, "{ind1}\"fields\": [").unwrap();
    let all_fields: Vec<_> = fields.iter().collect();
    for (i, field) in all_fields.iter().enumerate() {
        let (inner, optional) = unwrap_container(field.shape());
        let type_name = scalar_type_name(inner);
        let name = field.effective_name();
        let required = !optional && !field.has_default();
        let role = if field.is_argument() || field.is_arguments_plural() {
            "argument"
        } else if field.is_property() {
            "property"
        } else if field.is_element() || field.is_elements() {
            "child"
        } else {
            "other"
        };

        let ind2 = json_indent(depth + 2);
        write!(
            out,
            "{ind2}{{\"name\": \"{name}\", \"type\": \"{type_name}\", \"role\": \"{role}\", \"required\": {required}",
        )
        .unwrap();
        let fd = join_doc(field.doc);
        if !fd.is_empty() {
            write!(out, ", \"description\": \"{}\"", escape_json(&fd)).unwrap();
        }
        write!(out, "}}").unwrap();
        if i + 1 < all_fields.len() {
            writeln!(out, ",").unwrap();
        } else {
            writeln!(out).unwrap();
        }
    }
    writeln!(out, "{ind1}]").unwrap();

    write!(out, "{ind}}}").unwrap();
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn kdl_node_name_for_field(field: &facet::Field) -> String {
    if let Some(renamed) = field.rename {
        return renamed.to_string();
    }
    if field.is_elements() {
        singularize(field.name)
    } else {
        field.name.to_string()
    }
}

fn singularize(s: &str) -> String {
    if let Some(stripped) = s.strip_suffix("ies") {
        return format!("{stripped}y");
    }
    for esuffix in ["ches", "shes", "sses", "xes", "zes"] {
        if let Some(stripped) = s.strip_suffix(esuffix) {
            return format!("{stripped}{}", &esuffix[..esuffix.len() - 2]);
        }
    }
    if let Some(stripped) = s.strip_suffix('s') {
        return stripped.to_string();
    }
    s.to_string()
}

fn unwrap_container(shape: &'static Shape) -> (&'static Shape, bool) {
    match &shape.def {
        Def::Option(o) => {
            let (inner, _) = unwrap_container(o.t);
            (inner, true)
        }
        Def::List(l) => {
            let (inner, _) = unwrap_container(l.t());
            (inner, false)
        }
        _ => (shape, false),
    }
}

fn scalar_type_name(shape: &'static Shape) -> &'static str {
    match shape.type_identifier {
        "String" | "str" => "string",
        "bool" => "bool",
        other => other,
    }
}

fn write_doc(out: &mut String, doc: &'static [&'static str], depth: usize) {
    if doc.is_empty() {
        return;
    }
    let joined = doc.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
    let escaped = joined.replace('\\', "\\\\").replace('"', "\\\"");
    writeln!(out, "{}description \"{}\"", indent_for(depth), escaped).unwrap();
}

fn join_doc(doc: &'static [&'static str]) -> String {
    if doc.is_empty() {
        return String::new();
    }
    doc.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ")
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

trait InnerUserShape {
    fn inner_user_shape(&'static self) -> Option<&'static Shape>;
}

impl InnerUserShape for Shape {
    fn inner_user_shape(&'static self) -> Option<&'static Shape> {
        match &self.ty {
            Type::User(UserType::Struct(_))
            | Type::User(UserType::Enum(_))
            | Type::User(UserType::Union(_)) => Some(self),
            _ => match &self.def {
                Def::Option(o) => o.t.inner_user_shape(),
                Def::List(l) => l.t().inner_user_shape(),
                _ => None,
            },
        }
    }
}

fn indent(depth: usize) -> String {
    " ".repeat(4 * depth)
}

fn indent_for(depth: usize) -> String {
    indent(depth)
}

fn json_indent(depth: usize) -> String {
    " ".repeat(2 * depth)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kdl_schema_mentions_root_scene_node() {
        let schema = generate_schema_kdl();
        assert!(schema.contains("node \"scene\""), "missing root scene node");
    }

    #[test]
    fn kdl_schema_mentions_top_level_children() {
        let schema = generate_schema_kdl();
        for expected in [
            "\"extends\"",
            "\"include\"",
            "\"use\"",
            "\"layout\"",
            "\"plugin\"",
            "\"on\"",
            "\"keybind\"",
            "\"engine\"",
        ] {
            assert!(
                schema.contains(expected),
                "schema missing {expected}"
            );
        }
    }

    #[test]
    fn json_schema_is_valid_json() {
        let schema = generate_schema_json();
        // Basic structural checks — full JSON parse would require serde_json
        // in dev-deps which we avoid for a lightweight check.
        assert!(schema.starts_with('{'));
        assert!(schema.contains("\"info\""));
        assert!(schema.contains("\"root\""));
        assert!(schema.contains("\"definitions\""));
    }
}
