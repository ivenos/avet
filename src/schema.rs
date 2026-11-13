use crate::config::Config;
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
struct Section {
    intro: Option<String>,
    rows: BTreeMap<String, Row>,
}

struct Row {
    default: String,
    description: String,
}

fn readme() -> BTreeMap<String, Section> {
    let mut sections = BTreeMap::<String, Section>::new();
    let mut current = None;
    let mut in_code = false;
    for line in include_str!("../README.md").lines() {
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if line.starts_with('#') {
            current = line
                .strip_prefix("### `")
                .and_then(|l| l.strip_suffix('`'))
                .map(|name| name.trim_matches(['[', ']']).to_string());
            if let Some(name) = &current {
                sections.entry(name.clone()).or_default();
            }
            continue;
        }
        let Some(section) = current.as_ref().and_then(|name| sections.get_mut(name)) else {
            continue;
        };
        if line.starts_with("| `") {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            assert_eq!(cells.len(), 5, "README row with other than three cells: {line}");
            section.rows.insert(
                cells[1].trim_matches('`').to_string(),
                Row { default: cells[2].trim_matches('`').to_string(), description: cells[3].to_string() },
            );
        } else if section.intro.is_none() && section.rows.is_empty() && !line.is_empty() && !line.starts_with('|') {
            section.intro = Some(line.to_string());
        }
    }
    sections
}

fn generate() -> Value {
    let schema = SchemaSettings::draft07()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<Config>();
    let mut schema = serde_json::to_value(schema).unwrap();
    clean(&mut schema);
    let readme = readme();
    for (name, prop) in schema["properties"].as_object_mut().unwrap() {
        let section = readme.get(name).unwrap_or_else(|| panic!("README has no section for `{name}`"));
        if let Some(intro) = &section.intro {
            prop["description"] = json!(intro);
        }
        describe(prop, name, section);
    }
    schema
}

/// TOML has no null, and the descriptions come from the README rather than from doc comments.
fn clean(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(clean),
        Value::Object(map) => {
            map.values_mut().for_each(clean);
            for key in ["description", "title", "format"] {
                map.remove(key);
            }
            if let Some(Value::Array(types)) = map.get_mut("type") {
                types.retain(|t| t != "null");
                if let [only] = types.as_slice() {
                    let only = only.clone();
                    map.insert("type".into(), only);
                }
            }
            if let Some(Value::Array(values)) = map.get_mut("enum") {
                values.retain(|v| !v.is_null());
                map.remove("minimum");
                map.remove("maximum");
            }
            if let Some(Value::Array(branches)) = map.get_mut("anyOf") {
                branches.retain(|b| b != &json!({ "type": "null" }));
                if let [Value::Object(only)] = branches.as_slice() {
                    let only = only.clone();
                    map.remove("anyOf");
                    map.extend(only);
                }
            }
            if map.get("default").is_some_and(Value::is_null) || map.contains_key("properties") {
                map.remove("default");
            }
        }
        _ => {}
    }
}

fn is_table(prop: &Value) -> bool {
    prop.get("properties").is_some() || prop.get("additionalProperties").is_some_and(Value::is_object)
}

fn describe(table: &mut Value, path: &str, section: &Section) {
    if let Some(inner) = table.get_mut("additionalProperties").filter(|v| v.is_object()) {
        describe(inner, path, section);
    }
    let Some(Value::Object(props)) = table.get_mut("properties") else {
        return;
    };
    for (key, prop) in props.iter_mut() {
        let nested = is_table(prop);
        match section.rows.get(key) {
            Some(row) => prop["description"] = json!(row.description),
            None if nested => {}
            None => panic!("README has no row for `{path}.{key}`"),
        }
        if nested {
            describe(prop, &format!("{path}.{key}"), section);
        }
    }
}

#[test]
fn schema_is_written_to_schema_out() {
    let schema = serde_json::to_string_pretty(&generate()).unwrap() + "\n";
    if let Some(path) = std::env::var_os("SCHEMA_OUT") {
        std::fs::write(path, schema).unwrap();
    }
}

fn keys(table: &Value, out: &mut Vec<String>) {
    if let Some(inner) = table.get("additionalProperties").filter(|v| v.is_object()) {
        keys(inner, out);
    }
    for (key, prop) in table.get("properties").and_then(Value::as_object).into_iter().flatten() {
        out.push(key.clone());
        keys(prop, out);
    }
}

#[test]
fn every_readme_row_names_a_key() {
    let schema = generate();
    for (name, section) in readme() {
        let table = schema["properties"].get(&name).unwrap_or_else(|| panic!("README section `{name}` is no key"));
        let mut known = Vec::new();
        keys(table, &mut known);
        for key in section.rows.keys() {
            assert!(known.contains(key), "README row `{name}.{key}` is no key");
        }
    }
}

#[test]
fn readme_defaults_match_the_code() {
    let schema = generate();
    let mut checked = 0;
    for (name, section) in readme() {
        for (key, row) in &section.rows {
            let Some(prop) = schema["properties"][&name]["properties"].get(key) else {
                continue;
            };
            let documented = (row.default != "-").then(|| {
                let table: toml::Table = toml::from_str(&format!("v = {}", row.default))
                    .unwrap_or_else(|e| panic!("README default of `{name}.{key}` is no TOML value: {e}"));
                serde_json::to_value(&table["v"]).unwrap()
            });
            let same = match (prop.get("default"), documented.as_ref()) {
                (Some(Value::Number(code)), Some(Value::Number(doc))) => code.as_f64() == doc.as_f64(),
                (code, doc) => code == doc,
            };
            assert!(same, "`{name}.{key}`: README says {}, the code {:?}", row.default, prop.get("default"));
            checked += 1;
        }
    }
    assert!(checked > 20, "only {checked} defaults compared");
}

const BASE: &str = "encoder = \"svt-av1\"\n[audio]\ncodec = \"libopus\"\nbitrate = \"192k\"\n";

fn validate_accepts(path: &[&str], value: &Value) -> bool {
    let mut config: toml::Table = toml::from_str(BASE).unwrap();
    if path[0] == "target_quality" {
        config.insert("target_quality".into(), toml::Value::Table(toml::from_str("jod = 9.5").unwrap()));
    }
    let value: toml::Value = serde_json::from_value(value.clone()).unwrap();
    match path {
        [key] => {
            config.insert(key.to_string(), value);
        }
        [table, key] => {
            let table = config.entry(table.to_string()).or_insert_with(|| toml::Value::Table(Default::default()));
            table.as_table_mut().unwrap().insert(key.to_string(), value);
        }
        _ => unreachable!(),
    }
    Config::from_str_for_test(&toml::to_string(&config).unwrap()).is_ok()
}

fn boundary_cases(prop: &Value) -> (Vec<Value>, Vec<Value>) {
    let integer = prop["type"] == "integer";
    let step = if integer { 1.0 } else { 0.01 };
    let num = |x: f64| if integer { json!(x as i64) } else { json!(x) };
    let bound = |key: &str| prop.get(key).and_then(Value::as_f64);
    let (mut inside, mut outside) = (Vec::new(), Vec::new());
    for (key, dir, exclusive) in [
        ("minimum", -1.0, false),
        ("maximum", 1.0, false),
        ("exclusiveMinimum", -1.0, true),
        ("exclusiveMaximum", 1.0, true),
    ] {
        if let Some(b) = bound(key) {
            let (at, past) = (num(b), num(b + dir * step));
            if exclusive {
                outside.push(at);
                inside.push(num(b - dir * step));
            } else {
                inside.push(at);
                outside.push(past);
            }
        }
    }
    if let Some(Value::Array(values)) = prop.get("enum") {
        inside.extend(values.iter().cloned());
        if integer {
            let ints: Vec<i64> = values.iter().filter_map(Value::as_i64).collect();
            let (lo, hi) = (ints.iter().min().unwrap() - 1, ints.iter().max().unwrap() + 1);
            outside.extend((lo..=hi).filter(|i| !ints.contains(i)).map(|i| json!(i)));
        } else {
            outside.push(json!("unknown"));
        }
    }
    if let Some(Value::Array(branches)) = prop.get("anyOf") {
        let holds = |v: f64| branches.iter().any(|b| {
            b.get("const").and_then(Value::as_f64) == Some(v) || b.get("minimum").and_then(Value::as_f64).is_some_and(|m| v >= m)
        });
        for b in branches {
            for (key, near) in [("const", step), ("minimum", -step)] {
                if let Some(v) = b.get(key).and_then(Value::as_f64) {
                    inside.push(num(v));
                    if !holds(v + near) {
                        outside.push(num(v + near));
                    }
                }
            }
        }
    }
    (inside, outside)
}

#[test]
fn schema_bounds_match_validate() {
    let schema = generate();
    let mut checked = 0;
    for (name, prop) in schema["properties"].as_object().unwrap() {
        let mut fields = vec![(vec![name.as_str()], prop)];
        for (key, field) in prop.get("properties").and_then(Value::as_object).into_iter().flatten() {
            fields.push((vec![name.as_str(), key.as_str()], field));
        }
        for (path, field) in fields {
            let (inside, outside) = boundary_cases(field);
            for value in inside {
                assert!(validate_accepts(&path, &value), "{} = {value} fits the schema, avet rejects it", path.join("."));
                checked += 1;
            }
            for value in outside {
                assert!(!validate_accepts(&path, &value), "{} = {value} breaks the schema, avet accepts it", path.join("."));
                checked += 1;
            }
        }
    }
    assert!(checked > 40, "only {checked} values checked");
}
