use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[allow(non_snake_case)]
pub struct Config {
    pub inlayHints_closingBraceHints_enable: bool,

    #[serde(default = "min_lines_default")]
    pub inlayHints_closingBraceHints_minLines: u32,
}

fn min_lines_default() -> u32 {
    25
}

impl Config {
    pub fn from_json(value: Value) -> Result<Self, serde_json::Error> {
        let flattened_value = flatten_json(value);
        serde_json::from_value(flattened_value)
    }
}

fn flatten_json(value: Value) -> Value {
    fn flatten_object(obj: &Map<String, Value>, prefix: &str) -> Map<String, Value> {
        let mut flattened = Map::new();

        for (key, val) in obj {
            let new_key = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}_{key}")
            };

            match val {
                Value::Object(nested) => {
                    let nested_flattened = flatten_object(nested, &new_key);
                    flattened.extend(nested_flattened);
                }
                _ => {
                    flattened.insert(new_key, val.clone());
                }
            }
        }

        flattened
    }

    match value {
        Value::Object(obj) => Value::Object(flatten_object(&obj, "")),
        _ => value,
    }
}

#[cfg(test)]
mod test {
    use super::*;

    impl Config {
        // You manually maintain this - but validation will catch if you forget fields
        pub fn field_docs() -> Vec<(&'static str, &'static str, Option<&'static str>)> {
            vec![
            (
                "inlayHints_closingBraceHints_enable",
                "Display inlay hints next to closing `}` braces to show which block or construct they terminate.",
                Some("true"),
            ),
            (
                "inlayHints_closingBraceHints_minLines",
                "Required minimum line count between opening and closing braces before displaying hints (use 0 or 1 to display hints for all blocks).",
                Some("25"),
            ),
        ]
        }

        pub fn generate_markdown() -> String {
            let mut md = String::new();
            md.push_str("# Configuration\n\n");

            for (field, doc, default) in Self::field_docs() {
                md.push_str(&format!("## `{}`\n\n", field));
                md.push_str(&format!("{}\n\n", doc));
                if let Some(def) = default {
                    md.push_str(&format!("**Default:** `{}`\n\n", def));
                }
            }

            md
        }

        // Validation function
        pub fn validate_documentation() -> Result<(), String> {
            use serde_json::Value;
            use std::collections::HashSet;

            // Get all actual struct fields by serializing a default instance
            let instance = Self::default();
            let serialized = serde_json::to_value(instance)
                .map_err(|e| format!("Failed to serialize config: {}", e))?;

            let actual_fields: HashSet<&str> = if let Value::Object(map) = &serialized {
                map.keys().map(|s| s.as_str()).collect()
            } else {
                return Err("Config should serialize to an object".to_string());
            };

            // Get documented fields
            let documented_fields: HashSet<&str> = Self::field_docs()
                .iter()
                .map(|(field, _, _)| *field)
                .collect();

            // Find mismatches
            let missing_docs: Vec<&str> = actual_fields
                .difference(&documented_fields)
                .copied()
                .collect();

            let extra_docs: Vec<&str> = documented_fields
                .difference(&actual_fields)
                .copied()
                .collect();

            let mut errors = Vec::new();

            if !missing_docs.is_empty() {
                errors.push(format!(
                    "❌ Missing documentation for fields: {:?}",
                    missing_docs
                ));
            }

            if !extra_docs.is_empty() {
                errors.push(format!(
                    "❌ Documentation for non-existent fields: {:?}",
                    extra_docs
                ));
            }

            if errors.is_empty() {
                println!("✅ All fields are properly documented!");
                Ok(())
            } else {
                Err(errors.join("\n"))
            }
        }
    }

    #[test]
    fn test_default_config() -> eyre::Result<()> {
        let str = r#"
        {
            "inlayHints": {
               "closingBraceHints": {
                   "enable": true,
                   "minLines": 5
                }
            }
        }"#;

        let value: Value = serde_json::from_str(str)?;
        let config = Config::from_json(value)?;
        assert!(config.inlayHints_closingBraceHints_enable);
        assert_eq!(config.inlayHints_closingBraceHints_minLines, 5);

        Ok(())
    }

    #[test]
    fn test_config_documentation_complete() {
        if let Err(e) = Config::validate_documentation() {
            panic!("Documentation validation failed:\n{}", e);
        }
    }

    const CONFIG_PATH: &str = "docs/configuration.md";

    #[test]
    fn test_generate_config_markdown() {
        let check = std::env::var("ACTION").unwrap_or_else(|_| "generate".to_string());

        let markdown = Config::generate_markdown();
        if check == "generate" {
            std::fs::write(CONFIG_PATH, markdown)
                .expect("Failed to write config documentation to file");
        } else if check == "validate" {
            let expected = std::fs::read_to_string(CONFIG_PATH)
                .expect("Failed to read expected config documentation file");
            assert_eq!(
                markdown, expected,
                "Generated markdown does not match expected output"
            );
        } else {
            panic!("Unknown ACTION: {}", check);
        }
    }
}
