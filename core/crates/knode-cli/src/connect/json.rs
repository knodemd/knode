//! Adds or replaces one entry in a JSON settings file, changing nothing else: the rest of the
//! file keeps its order, spacing and, where the agent allows them, its comments.

use anyhow::{Context, Result, anyhow, bail};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstRootNode};
use serde_json::{Map, Value};

/// What an agent accepts in its settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// Plain JSON. A file with comments is refused, since the agent itself could not read it.
    Json,
    /// JSON with comments and trailing commas, as VS Code and Zed write it. Comments are kept.
    Jsonc,
}

/// The settings with `<container>.<name>` set to `entry`, or `None` when it already is.
/// `container` is one key of the top-level object, such as `mcpServers` (a key may contain dots,
/// like Amp's `amp.mcpServers`: it is never split).
pub fn set_entry(
    text: &str,
    syntax: Syntax,
    container: &str,
    name: &str,
    entry: &Value,
) -> Result<Option<String>> {
    if text.trim().is_empty() {
        let mut servers = Map::new();
        servers.insert(name.to_string(), entry.clone());
        let mut root = Map::new();
        root.insert(container.to_string(), Value::Object(servers));
        let mut out = serde_json::to_string_pretty(&Value::Object(root))?;
        out.push('\n');
        return Ok(Some(out));
    }
    if syntax == Syntax::Json
        && let Err(e) = serde_json::from_str::<Value>(text)
    {
        let hint = if has_comment(text) {
            " (it has comments, which this agent does not read)"
        } else {
            ""
        };
        bail!("it is not valid JSON{hint}: {e}; fix it and try again");
    }
    let root = CstRootNode::parse(text, &options())
        .map_err(|e| anyhow!("it is not valid JSON: {e}; fix it and try again"))?;
    let object = root
        .object_value()
        .context("it is not a JSON object; fix it and try again")?;
    let servers = match object.get(container) {
        Some(prop) => prop.object_value().with_context(|| {
            format!("\"{container}\" is not a JSON object; fix it and try again")
        })?,
        None => object
            .append(container, CstInputValue::Object(Vec::new()))
            .object_value()
            .context("cannot add the settings")?,
    };
    match servers.get(name) {
        Some(prop) => {
            let current = prop.value().and_then(|v| v.to_serde_value());
            if current.as_ref() == Some(entry) {
                return Ok(None);
            }
            prop.set_value(input(entry));
        }
        None => {
            servers.append(name, input(entry));
        }
    }
    Ok(Some(root.to_string()))
}

/// The value of `<container>.<name>`, if the file has it.
pub fn get_entry(text: &str, container: &str, name: &str) -> Option<Value> {
    let root = CstRootNode::parse(text, &options()).ok()?;
    root.object_value()?
        .object_value(container)?
        .get(name)?
        .value()?
        .to_serde_value()
}

/// JSON, plus comments and trailing commas. Nothing looser (single quotes, bare keys, hex
/// numbers): no agent reads those, so a file using them is refused.
fn options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}

/// Whether a text that is not valid JSON looks like it has comments.
fn has_comment(text: &str) -> bool {
    let options = ParseOptions {
        allow_comments: false,
        ..options()
    };
    jsonc_parser::parse_to_value(text, &options).is_err()
        && jsonc_parser::parse_to_value(text, &self::options()).is_ok()
}

fn input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(a) => CstInputValue::Array(a.iter().map(input).collect()),
        Value::Object(o) => {
            CstInputValue::Object(o.iter().map(|(k, v)| (k.clone(), input(v))).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry() -> Value {
        json!({ "command": "/bin/knode", "args": ["mcp", "--agent", "zed"] })
    }

    #[test]
    fn keeps_comments_and_layout() {
        let text = "// Zed settings\n{\n  \"theme\": \"One Dark\", // the theme\n  \"context_servers\": {\n    // mine\n    \"other\": { \"command\": \"x\" },\n  },\n}\n";
        let out = set_entry(text, Syntax::Jsonc, "context_servers", "knode", &entry())
            .unwrap()
            .unwrap();
        assert!(out.starts_with("// Zed settings\n{\n  \"theme\": \"One Dark\", // the theme\n"));
        assert!(
            out.contains("    // mine\n    \"other\": { \"command\": \"x\" },"),
            "{out}"
        );
        assert_eq!(get_entry(&out, "context_servers", "knode"), Some(entry()));
        assert_eq!(
            set_entry(&out, Syntax::Jsonc, "context_servers", "knode", &entry()).unwrap(),
            None
        );
        assert!(out.ends_with("}\n"), "{out}");
    }

    #[test]
    fn replaces_only_its_own_entry() {
        let text = r#"{"mcpServers": {"knode": {"command": "old"}, "b": {"command": "y"}}}"#;
        let out = set_entry(text, Syntax::Json, "mcpServers", "knode", &entry())
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["mcpServers"]["knode"], entry());
        assert_eq!(value["mcpServers"]["b"]["command"], "y");
        let keys: Vec<&String> = value["mcpServers"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["knode", "b"]);
    }

    #[test]
    fn dotted_keys_are_one_key() {
        let out = set_entry("{}", Syntax::Jsonc, "amp.mcpServers", "knode", &entry())
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["amp.mcpServers"]["knode"], entry());
        let fresh = set_entry("", Syntax::Json, "amp.mcpServers", "knode", &entry())
            .unwrap()
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&fresh).unwrap(), value);
    }

    #[test]
    fn refuses_what_the_agent_could_not_read() {
        let err = set_entry("// hi\n{}", Syntax::Json, "mcpServers", "knode", &entry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not valid JSON (it has comments"), "{err}");
        for bad in ["{nope", "[1]", "{'a': 1}", "{a: 1}", r#"{"mcpServers": 3}"#] {
            assert!(
                set_entry(bad, Syntax::Jsonc, "mcpServers", "knode", &entry()).is_err(),
                "{bad}"
            );
        }
    }
}
