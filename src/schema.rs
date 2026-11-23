use crate::config::Config;
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
struct Section {
    intro: Option<String>,
    rows: BTreeMap<String, Row>,
    grouped: BTreeMap<String, String>,
    overrides: Option<Box<Section>>,
}

struct Row {
    default: String,
    values: String,
    description: String,
}

const OVERRIDES_HEADING: &str = "### `[audio.lossless]` and `[audio.codec_rules]`";

fn readme() -> BTreeMap<String, Section> {
    let mut sections = BTreeMap::<String, Section>::new();
    let mut current = None;
    let mut in_overrides = false;
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
            in_overrides = line == OVERRIDES_HEADING;
            current = if in_overrides {
                Some("audio".to_string())
            } else {
                line.strip_prefix("### `")
                    .and_then(|l| l.strip_suffix('`'))
                    .filter(|name| !name.contains('`'))
                    .map(|name| name.trim_matches(['[', ']']).to_string())
            };
            if let Some(name) = &current {
                sections.entry(name.clone()).or_default();
            }
            continue;
        }
        let Some(section) = current.as_ref().and_then(|name| sections.get_mut(name)) else {
            continue;
        };
        if in_overrides {
            if line.starts_with("| `") {
                let cells: Vec<&str> = line.split('|').map(str::trim).collect();
                assert_eq!(cells.len(), 6, "README row with other than four cells: {line}");
                let overrides = section.overrides.get_or_insert_default();
                for key in cells[1].split(',') {
                    overrides.rows.insert(
                        key.trim().trim_matches('`').to_string(),
                        Row {
                            default: cells[2].to_string(),
                            values: cells[3].to_string(),
                            description: format!("{}. Default: {}. Values: {}", cells[4], cells[2], cells[3]),
                        },
                    );
                }
            } else if let Some((table, _)) = line.strip_prefix("- `[audio.").and_then(|l| l.split_once("]`")) {
                section.grouped.insert(table.to_string(), line.trim_start_matches("- ").to_string());
            }
            continue;
        }
        if line.starts_with("| color keys |") {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            assert_eq!(cells.len(), 6, "README row with other than four cells: {line}");
            let kind = cells[3].split(',').next().unwrap_or_default();
            for key in crate::config::COLOR_KEYS {
                let example = crate::config::color_key_example(key);
                section.grouped.insert(key.to_string(), format!("{}. Values: {kind}, such as {example}", cells[4]));
            }
        } else if line.starts_with("| `") {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            assert_eq!(cells.len(), 6, "README row with other than four cells: {line}");
            assert!(!cells[3].is_empty() && !cells[4].is_empty() && !cells[4].ends_with('.'), "README row with an empty cell or a final period: {line}");
            section.rows.insert(
                cells[1].trim_matches('`').to_string(),
                Row {
                    default: cells[2].trim_matches('`').to_string(),
                    values: cells[3].to_string(),
                    description: format!("{}. Values: {}", cells[4], cells[3]),
                },
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
        if let Some(text) = section.intro.as_ref().or(section.rows.get(name).map(|row| &row.description)) {
            prop["description"] = json!(text);
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
    let open = table.get("additionalProperties") == Some(&Value::Bool(true));
    let Some(Value::Object(props)) = table.get_mut("properties") else {
        return;
    };
    for (key, prop) in props.iter_mut() {
        let nested = is_table(prop);
        match section.rows.get(key).map(|row| &row.description).or(section.grouped.get(key)) {
            Some(text) => {
                prop["description"] = json!(text);
                if section.grouped.contains_key(key)
                    && let Some(entry) = prop.get_mut("additionalProperties").filter(|v| v.is_object())
                {
                    entry["description"] = json!(text);
                }
            }
            None if nested || open => {}
            None => panic!("README has no row for `{path}.{key}`"),
        }
        if nested {
            let inner = section.overrides.as_deref().filter(|_| section.grouped.contains_key(key));
            describe(prop, &format!("{path}.{key}"), inner.unwrap_or(section));
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

#[test]
fn audio_overrides_are_described_as_inheriting_from_audio() {
    let schema = generate();
    let audio = &schema["properties"]["audio"]["properties"];
    for (table, keys) in [("lossless", &audio["lossless"]), ("codec_rules", &audio["codec_rules"]["additionalProperties"])] {
        let text = audio[table]["description"].as_str().unwrap_or_else(|| panic!("audio.{table} has no description"));
        assert!(text.contains(&format!("[audio.{table}]")), "audio.{table}: {text}");
        assert_eq!(keys["description"].as_str(), Some(text), "an entry of audio.{table}");
        let keys = keys["properties"].as_object().unwrap();
        assert_eq!(keys.keys().collect::<Vec<_>>(), ["bitrate", "codec", "mode", "options"]);
        for (key, prop) in keys {
            let text = prop["description"].as_str().unwrap_or_else(|| panic!("audio.{table}.{key} has no description"));
            assert!(text.contains("Default: from `[audio]`") && !text.contains("Required"), "audio.{table}.{key}: {text}");
        }
    }
}

#[test]
fn every_color_key_has_the_description_of_the_readme_row() {
    let schema = generate();
    for key in crate::config::COLOR_KEYS {
        let text = schema["properties"]["encoder_params"]["properties"][key]["description"].as_str();
        assert!(text.is_some_and(|t| t.starts_with("Replace the color description")), "{key}: {text:?}");
        assert!(text.is_some_and(|t| t.ends_with(&format!("such as {}", crate::config::color_key_example(key)))), "{key}: {text:?}");
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
        for key in section.rows.keys().filter(|key| **key != name) {
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
            let prop = if *key == name { schema["properties"].get(key) } else { schema["properties"][&name]["properties"].get(key) };
            let Some(prop) = prop else {
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

#[test]
fn readme_values_name_the_schema_bounds() {
    let schema = generate();
    let text = |v: &Value| v.as_f64().filter(|f| f.fract() == 0.0).map_or_else(|| v.to_string(), |f| (f as i64).to_string());
    let mut checked = 0;
    for (name, section) in readme() {
        for (key, row) in &section.rows {
            let prop = if *key == name { &schema["properties"][key] } else { &schema["properties"][&name]["properties"][key] };
            let linked = section.rows.keys().any(|other| other != key && row.values.contains(&format!("`{other}`")));
            let mut named: Vec<&Value> = prop.get("enum").and_then(Value::as_array).into_iter().flatten().collect();
            if !linked {
                named.extend(["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"].iter().filter_map(|b| prop.get(*b)));
            }
            for value in named {
                assert!(row.values.contains(&format!("`{}`", text(value))), "README values of `{name}.{key}` leave out {value}: {}", row.values);
                checked += 1;
            }
        }
    }
    assert!(checked > 20, "only {checked} values compared");
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
    let integer = match &prop["type"] {
        Value::Array(types) => types.contains(&json!("integer")),
        t => t == "integer",
    };
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

fn fits(schema: &Value, value: &Value) -> bool {
    let Some(schema) = schema.as_object() else {
        return schema != &Value::Bool(false);
    };
    let typed = |t: &Value| match t.as_str() {
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("number") => value.is_number(),
        Some("string") => value.is_string(),
        Some("boolean") => value.is_boolean(),
        Some("array") => value.is_array(),
        Some("object") => value.is_object(),
        _ => false,
    };
    let bound = |key: &str, holds: fn(f64, f64) -> bool| {
        schema.get(key).and_then(Value::as_f64).is_none_or(|b| value.as_f64().is_none_or(|v| holds(v, b)))
    };
    let pattern = schema.get("pattern").and_then(Value::as_str).is_none_or(|p| {
        value.as_str().is_none_or(|text| regex_automata::meta::Regex::new(p).unwrap().is_match(text))
    });
    let items = schema.get("items").is_none_or(|s| value.as_array().is_none_or(|a| a.iter().all(|v| fits(s, v))));
    let members = value.as_object().is_none_or(|object| {
        let props = schema.get("properties").and_then(Value::as_object);
        let required = schema.get("required").and_then(Value::as_array).into_iter().flatten();
        required.filter_map(Value::as_str).all(|k| object.contains_key(k))
            && object.iter().all(|(k, v)| match props.and_then(|p| p.get(k)) {
                Some(s) => fits(s, v),
                None => schema.get("additionalProperties").is_none_or(|s| fits(s, v)),
            })
    });
    schema.get("type").is_none_or(|t| t.as_array().map_or_else(|| typed(t), |ts| ts.iter().any(typed)))
        && schema.get("enum").and_then(Value::as_array).is_none_or(|e| e.contains(value))
        && schema.get("const").is_none_or(|c| c == value)
        && schema.get("anyOf").and_then(Value::as_array).is_none_or(|b| b.iter().any(|b| fits(b, value)))
        && bound("minimum", |v, b| v >= b)
        && bound("maximum", |v, b| v <= b)
        && bound("exclusiveMinimum", |v, b| v > b)
        && bound("exclusiveMaximum", |v, b| v < b)
        && pattern
        && items
        && members
}

#[test]
fn the_schema_takes_the_values_validate_takes() {
    let schema = generate();
    let node = |path: &[&str]| path.iter().fold(&schema, |s, key| &s["properties"][key]);
    let languages = [
        json!(["eng"]), json!([" deu "]), json!(["por-BR", "deu_DE"]), json!(["ENG"]), json!([]),
        json!(["en"]), json!(["de-DE"]), json!([""]), json!(["english"]), json!(["e1g"]), json!(["eng", "de"]),
    ];
    let bitrates = [
        json!("192k"), json!("192K"), json!(" 256k "), json!("1.5k"), json!("192000"), json!("1000"), json!("1M"), json!("0.001M"),
        json!("192"), json!("999"), json!("0.5k"), json!("0.0009M"), json!("loud"), json!("1m"), json!("k"), json!(""), json!(192),
        json!("+192k"), json!("1e3"), json!("2e2k"),
        json!({ "stereo": "192k", "5.1": "320k", "default": "128k" }), json!({ "5,1": "320k" }), json!({ "stereo": "128" }),
        json!({ "stereo": 128 }),
    ];
    let mut cases: Vec<(Vec<&str>, Value)> = Vec::new();
    for section in ["audio", "subtitles"] {
        cases.extend(languages.iter().map(|v| (vec![section, "language_whitelist"], v.clone())));
    }
    for v in &bitrates {
        cases.push((vec!["audio", "bitrate"], v.clone()));
        cases.push((vec!["audio", "lossless"], json!({ "bitrate": v })));
        cases.push((vec!["audio", "codec_rules"], json!({ "opus": { "bitrate": v } })));
    }
    for v in [json!(30), json!(27.5), json!("30"), json!("-1"), json!("1e3"), json!("thirty"), json!(""), json!("nan"), json!("inf"), json!(true)] {
        cases.push((vec!["encoder_params", "crf"], v));
    }
    for key in crate::config::COLOR_KEYS {
        for v in [json!(9), json!(0), json!("16"), json!("+9"), json!(-1), json!(9.5), json!(9.0), json!(true), json!("bt2020"), json!(""), json!([1])] {
            cases.push((vec!["encoder_params", key], v));
        }
    }
    for name in schema["properties"].as_object().unwrap().keys().filter(|k| is_table(&schema["properties"][k])) {
        cases.push((vec![name.as_str()], json!({})));
    }
    for (path, value) in cases {
        let field = if path.len() == 1 { &schema["properties"][path[0]] } else { node(&path) };
        let (editor, avet) = (fits(field, &value), validate_accepts(&path, &value));
        assert_eq!(editor, avet, "{} = {value}: the schema {}, avet {}", path.join("."),
            if editor { "takes it" } else { "refuses it" }, if avet { "takes it" } else { "refuses it" });
    }
}
