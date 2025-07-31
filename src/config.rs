use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Deserialize, Default)]
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
                format!("{}_{}", prefix, key)
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
}
