use serde_json::{Map, Value, json};

use crate::tool::{ToolDefinition, ToolFunction};

pub(super) fn tool(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: name.into(),
            description: description.into(),
            parameters,
        },
    }
}

pub(super) fn no_args_tool(name: &str, description: &str) -> ToolDefinition {
    tool(name, description, object(Vec::new(), &[]))
}

pub(super) fn object(properties: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let properties = properties
        .into_iter()
        .map(|(name, schema)| (name.to_string(), schema))
        .collect::<Map<String, Value>>();

    let mut schema = Map::new();
    schema.insert("type".into(), Value::String("object".into()));
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), json!(required));
    }
    Value::Object(schema)
}

pub(super) fn string(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description
    })
}

pub(super) fn string_enum(description: &str, values: &[&str]) -> Value {
    json!({
        "type": "string",
        "enum": values,
        "description": description
    })
}

pub(super) fn integer(description: &str) -> Value {
    json!({
        "type": "integer",
        "description": description
    })
}

pub(super) fn integer_item() -> Value {
    json!({
        "type": "integer"
    })
}

pub(super) fn boolean(description: &str) -> Value {
    json!({
        "type": "boolean",
        "description": description
    })
}

pub(super) fn array(description: &str, items: &Value) -> Value {
    json!({
        "type": "array",
        "items": items,
        "description": description
    })
}

pub(super) fn with_default(mut schema: Value, default: Value) -> Value {
    if let Value::Object(object) = &mut schema {
        object.insert("default".into(), default);
    }
    schema
}
