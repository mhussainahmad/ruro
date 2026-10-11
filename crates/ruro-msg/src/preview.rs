use serde::{Deserialize, Serialize};

/// Upper bound on the number of numeric fields a preview carries.
const MAX_VALUES: usize = 32;

/// Display-oriented summary of a message, produced on demand for tooling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Preview {
    /// Downscaled RGBA8 image.
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// Named numeric values, suitable for plotting.
    Values(Vec<(String, f64)>),
    /// Free-form text.
    Text(String),
}

impl Preview {
    /// Builds a preview from any serializable value: numeric leaves become
    /// plottable values, anything else falls back to JSON text.
    pub fn from_serialize<T: Serialize + ?Sized>(value: &T) -> Preview {
        match serde_json::to_value(value) {
            Ok(json) => {
                let mut values = Vec::new();
                flatten_json("", &json, &mut values);
                if values.is_empty() {
                    Preview::Text(json.to_string())
                } else {
                    Preview::Values(values)
                }
            }
            Err(err) => Preview::Text(format!("<unpreviewable: {err}>")),
        }
    }
}

/// Collects numeric (and boolean) leaves of a JSON value as `path = number` pairs.
pub fn flatten_json(prefix: &str, value: &serde_json::Value, out: &mut Vec<(String, f64)>) {
    use serde_json::Value;
    if out.len() >= MAX_VALUES {
        return;
    }
    let join = |key: &str| {
        if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        }
    };
    match value {
        Value::Number(n) => {
            if let Some(v) = n.as_f64() {
                let name = if prefix.is_empty() { "value" } else { prefix };
                out.push((name.to_string(), v));
            }
        }
        Value::Bool(b) => {
            let name = if prefix.is_empty() { "value" } else { prefix };
            out.push((name.to_string(), f64::from(u8::from(*b))));
        }
        Value::Object(map) => {
            for (k, v) in map {
                flatten_json(&join(k), v, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                flatten_json(&format!("{prefix}[{i}]"), v, out);
            }
        }
        Value::Null | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Pose {
        x: f64,
        y: f64,
        joints: [f32; 2],
        label: &'static str,
    }

    #[test]
    fn flattens_numeric_leaves() {
        let p = Pose {
            x: 1.0,
            y: -2.5,
            joints: [0.5, 0.25],
            label: "a",
        };
        let Preview::Values(values) = Preview::from_serialize(&p) else {
            panic!("expected values");
        };
        assert_eq!(
            values,
            vec![
                ("x".into(), 1.0),
                ("y".into(), -2.5),
                ("joints[0]".into(), 0.5),
                ("joints[1]".into(), 0.25),
            ]
        );
    }

    #[test]
    fn plain_number_is_named_value() {
        assert_eq!(
            Preview::from_serialize(&3.0f64),
            Preview::Values(vec![("value".into(), 3.0)])
        );
    }

    #[test]
    fn text_fallback() {
        assert_eq!(
            Preview::from_serialize("hi"),
            Preview::Text("\"hi\"".into())
        );
    }
}
