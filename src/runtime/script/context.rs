//! Script preprocessing and JavaScript-JSON conversion.
//!
//! Handles transforming user scripts to executable form and converting
//! `JavaScript` values to/from JSON.

use rquickjs::{FromJs, Object, Value as JsValue};

/// Preprocess a script to support `export default function(input) { ... }` syntax.
///
/// Transforms:
/// ```js
/// export default function(input) { return input.value; }
/// ```
/// Into:
/// ```js
/// var __default__ = function(input) { return input.value; };
/// __default__(input);
/// ```
///
/// For async functions, wraps the call in Promise.then to capture the result.
/// The runtime will run pending jobs to resolve the Promise.
pub(crate) fn preprocess_script(script: &str) -> String {
    let is_async = script.contains("export default async");

    // Replace "export default" with variable assignment
    let transformed = script
        .replace(
            "export default async function",
            "var __default__ = async function",
        )
        .replace("export default function", "var __default__ = function")
        .replace(
            "export default async (",
            "var __default__ = async function(",
        )
        .replace("export default (", "var __default__ = function(");

    if is_async {
        // For async functions, use Promise.then to capture the result.
        // The runtime will execute pending jobs to resolve the Promise.
        format!(
            "{transformed}\n\
             var __async_result__ = {{ resolved: false, value: null, error: null }};\n\
             __default__(input).then(\n\
               function(r) {{ __async_result__.resolved = true; __async_result__.value = r; }},\n\
               function(e) {{ __async_result__.resolved = true; __async_result__.error = e ? e.toString() : 'Unknown error'; }}\n\
             );\n\
             __async_result__;"
        )
    } else {
        // Synchronous: just call and return
        format!("{transformed}\n__default__(input);")
    }
}

/// Convert a `JavaScript` value to a JSON value.
///
/// # Note on `needless_pass_by_value`
/// rquickjs `JsValue` must be passed by value because the conversion process may
/// consume the value (e.g., when extracting strings or iterating arrays). The JS
/// runtime's GC expects clear ownership semantics at the FFI boundary. Additionally,
/// `JsValue::clone()` is cheap (reference-counted internally), so callers can clone
/// if they need to retain the value.
#[allow(clippy::needless_pass_by_value)] // rquickjs JsValue ownership required for safe FFI conversion
pub(crate) fn js_to_json<'js>(
    ctx: &rquickjs::Ctx<'js>,
    value: JsValue<'js>,
) -> std::result::Result<serde_json::Value, String> {
    if value.is_null() || value.is_undefined() {
        Ok(serde_json::Value::Null)
    } else if let Some(b) = value.as_bool() {
        Ok(serde_json::Value::Bool(b))
    } else if let Some(i) = value.as_int() {
        Ok(serde_json::Value::Number(i.into()))
    } else if let Some(f) = value.as_float() {
        Ok(serde_json::Number::from_f64(f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number))
    } else if let Ok(s) = String::from_js(ctx, value.clone()) {
        Ok(serde_json::Value::String(s))
    } else if let Ok(arr) = rquickjs::Array::from_js(ctx, value.clone()) {
        let mut json_arr = Vec::new();
        for i in 0..arr.len() {
            if let Ok(item) = arr.get::<JsValue<'_>>(i) {
                json_arr.push(js_to_json(ctx, item)?);
            }
        }
        Ok(serde_json::Value::Array(json_arr))
    } else if let Ok(obj) = Object::from_js(ctx, value.clone()) {
        let mut json_obj = serde_json::Map::new();
        for (key, val) in obj.props::<String, JsValue<'_>>().flatten() {
            json_obj.insert(key, js_to_json(ctx, val)?);
        }
        Ok(serde_json::Value::Object(json_obj))
    } else {
        Ok(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rquickjs::{Context as JsContext, Runtime};
    use serde_json::json;

    #[test]
    fn test_preprocess_export_default_function() {
        let script = "export default function(input) { return input.value * 2; }";
        let processed = preprocess_script(script);
        assert!(processed.contains("var __default__ = function"));
        assert!(processed.contains("__default__(input);"));
        assert!(!processed.contains("export default"));
    }

    #[test]
    fn test_preprocess_export_default_async_function() {
        let script = "export default async function(input) { return await something(); }";
        let processed = preprocess_script(script);
        assert!(processed.contains("var __default__ = async function"));
        // Async functions are wrapped with Promise.then for proper resolution
        assert!(processed.contains("__async_result__"));
        assert!(processed.contains("__default__(input).then"));
    }

    #[test]
    fn test_preprocess_export_default_arrow() {
        let script = "export default (input) => { return input.name; }";
        let processed = preprocess_script(script);
        assert!(processed.contains("var __default__ = function(input)"));
        assert!(processed.contains("__default__(input);"));
    }

    #[test]
    fn test_preprocess_export_default_executes() {
        let script = "export default function(input) { return input.x + input.y; }";
        let processed = preprocess_script(script);

        // Execute the preprocessed script
        let runtime = Runtime::new().unwrap();
        let context = JsContext::full(&runtime).unwrap();

        let result = context
            .with(|ctx| {
                ctx.eval::<(), _>("var input = {x: 10, y: 5};").unwrap();
                let result: JsValue<'_> = ctx.eval(processed).unwrap();
                js_to_json(&ctx, result)
            })
            .unwrap();

        assert_eq!(result, json!(15));
    }

    #[test]
    fn test_preprocess_named_function() {
        let script = "export default function handler(input) { return {handled: true}; }";
        let processed = preprocess_script(script);
        assert!(processed.contains("var __default__ = function handler(input)"));

        let runtime = Runtime::new().unwrap();
        let context = JsContext::full(&runtime).unwrap();

        let result = context
            .with(|ctx| {
                ctx.eval::<(), _>("var input = {};").unwrap();
                let result: JsValue<'_> = ctx.eval(processed).unwrap();
                js_to_json(&ctx, result)
            })
            .unwrap();

        assert_eq!(result, json!({"handled": true}));
    }
}
