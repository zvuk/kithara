//! Bounded admission before Velato discards unsupported authored features.

use std::collections::BTreeMap;

use kithara_ui_draw::MAX_STOPS;
use serde_json::{Map, Value};
use velato::Composition;

use crate::{ArtworkError, consts};

pub(crate) fn parse(bytes: &[u8]) -> Result<Composition, ArtworkError> {
    if bytes.len() > consts::MAX_BYTES {
        return Err(ArtworkError::Limit {
            kind: "bytes",
            max: consts::MAX_BYTES,
        });
    }
    let document: Value = serde_json::from_slice(bytes).map_err(velato::Error::from)?;
    let mut remaining = consts::MAX_VALUES;
    check_value(&document, "$", 0, &mut remaining)?;
    for field in ["fr", "w", "h"] {
        if !document[field].as_f64().is_some_and(|value| value > 0.0) {
            return Err(invalid(field, "must be positive"));
        }
    }
    if !document["ip"]
        .as_f64()
        .zip(document["op"].as_f64())
        .is_some_and(|(start, end)| start < end)
    {
        return Err(invalid("ip/op", "must describe a nonempty frame interval"));
    }
    for field in ["assets", "fonts", "chars"] {
        if nonempty(&document[field]) {
            return Err(unsupported(field, "external assets or text"));
        }
    }
    check_parents(&document["layers"])?;
    Composition::from_json(document).map_err(ArtworkError::from)
}

fn check_value(
    value: &Value,
    path: &str,
    depth: usize,
    remaining: &mut usize,
) -> Result<(), ArtworkError> {
    if depth > consts::MAX_DEPTH {
        return Err(ArtworkError::Limit {
            kind: "JSON nesting",
            max: consts::MAX_DEPTH,
        });
    }
    *remaining = remaining.checked_sub(1).ok_or(ArtworkError::Limit {
        kind: "JSON values",
        max: consts::MAX_VALUES,
    })?;
    match value {
        Value::Number(number) => {
            if !number
                .as_f64()
                .is_some_and(|value| value.is_finite() && value.abs() <= f64::from(f32::MAX))
            {
                return Err(invalid(
                    path,
                    "number exceeds the finite drawing representation",
                ));
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                check_value(value, &format!("{path}[{index}]"), depth + 1, remaining)?;
            }
        }
        Value::Object(object) => {
            check_object(object, path)?;
            for (key, value) in object {
                check_value(value, &format!("{path}.{key}"), depth + 1, remaining)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
    Ok(())
}

fn check_object(object: &Map<String, Value>, path: &str) -> Result<(), ArtworkError> {
    let get = |key: &str| object.get(key).unwrap_or(&Value::Null);
    match get("ty") {
        Value::String(shape) => match shape.as_str() {
            "gr" | "tr" | "sh" | "rc" | "el" | "fl" | "st" | "gf" => {}
            _ => return Err(unsupported(path, "shape operation")),
        },
        Value::Number(layer) if !matches!(layer.as_u64(), Some(3 | 4)) => {
            return Err(unsupported(path, "layer type"));
        }
        _ => {}
    }
    for field in ["ddd", "bm", "tt", "td", "ao"] {
        if get(field).as_f64().is_some_and(|value| value != 0.0) {
            return Err(unsupported(
                &format!("{path}.{field}"),
                "3D, blending, matte or auto-orientation",
            ));
        }
    }
    for field in ["masksProperties", "ef"] {
        if nonempty(get(field)) {
            return Err(unsupported(&format!("{path}.{field}"), "masks or effects"));
        }
    }
    check_motion(object, path)?;
    check_layer(object, path)?;
    check_shape(object, path)
}

fn check_motion(object: &Map<String, Value>, path: &str) -> Result<(), ArtworkError> {
    let get = |key: &str| object.get(key).unwrap_or(&Value::Null);
    if get("x").is_string() {
        return Err(unsupported(path, "expressions"));
    }
    if get("s").as_bool() == Some(true) && (object.contains_key("x") || object.contains_key("y")) {
        return Err(unsupported(path, "split position"));
    }
    if get("r").get("or").is_some() {
        return Err(unsupported(path, "split rotation"));
    }
    for field in ["ti", "to"] {
        if get(field)
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value.as_f64() != Some(0.0)))
        {
            return Err(unsupported(path, "spatial position tangents"));
        }
    }
    Ok(())
}

fn check_layer(object: &Map<String, Value>, path: &str) -> Result<(), ArtworkError> {
    let get = |key: &str| object.get(key).unwrap_or(&Value::Null);
    if get("ty").is_number() {
        if get("ks")["r"].is_null() {
            return Err(invalid(path, "layer transform requires a rotation"));
        }
        if nonempty(get("tm")) {
            return Err(unsupported(path, "time remapping"));
        }
        if get("sr").as_f64().is_some_and(|value| value != 1.0)
            || get("st").as_f64().is_some_and(|value| value != 0.0)
        {
            return Err(unsupported(path, "layer time offset or stretch"));
        }
    }
    Ok(())
}

fn check_shape(object: &Map<String, Value>, path: &str) -> Result<(), ArtworkError> {
    let get = |key: &str| object.get(key).unwrap_or(&Value::Null);
    if matches!(get("ty").as_str(), Some("fl" | "gf"))
        && get("r").as_u64().is_some_and(|rule| rule != 1)
    {
        return Err(unsupported(path, "non-default Lottie fill rule"));
    }
    if get("ty").as_str() == Some("st") {
        if nonempty(get("d")) {
            return Err(unsupported(path, "stroke dashes"));
        }
        if get("ml").as_f64().is_some_and(|value| value != 4.0) {
            return Err(unsupported(path, "custom stroke miter limit"));
        }
    }
    if let Some(gradient) = get("g").as_object() {
        let count = gradient
            .get("p")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .filter(|count| (2..=MAX_STOPS).contains(count))
            .ok_or_else(|| invalid(path, "gradient stop count is outside the draw contract"))?;
        check_stops(&get("g")["k"]["k"], count, path)?;
    }
    if get("v").is_array() && get("i").is_array() && get("o").is_array() {
        let vertices = get("v").as_array().map(Vec::len);
        if vertices != get("i").as_array().map(Vec::len)
            || vertices != get("o").as_array().map(Vec::len)
        {
            return Err(invalid(
                path,
                "path vertices and tangents must have matching lengths",
            ));
        }
    }
    Ok(())
}

fn check_stops(value: &Value, count: usize, path: &str) -> Result<(), ArtworkError> {
    let values = value
        .as_array()
        .ok_or_else(|| invalid(path, "expected gradient values"))?;
    if values.first().is_some_and(Value::is_object) {
        for keyframe in values {
            for field in ["s", "e"] {
                if let Some(stops) = keyframe.get(field) {
                    check_stops(stops, count, path)?;
                }
            }
        }
        return Ok(());
    }
    if values.len() < count * 4 || !values.iter().all(Value::is_number) {
        return Err(invalid(path, "gradient colour-stop data is incomplete"));
    }
    let mut previous = 0.0;
    for stop in values.as_chunks::<4>().0.iter().take(count) {
        let offset = stop[0]
            .as_f64()
            .ok_or_else(|| invalid(path, "invalid gradient offset"))?;
        if !(0.0..=1.0).contains(&offset) || offset < previous {
            return Err(invalid(
                path,
                "gradient offsets must be ordered inside 0..=1",
            ));
        }
        previous = offset;
    }
    Ok(())
}

fn check_parents(layers: &Value) -> Result<(), ArtworkError> {
    let Some(layers) = layers.as_array() else {
        return Err(invalid("layers", "expected an array"));
    };
    let mut parents = BTreeMap::new();
    for layer in layers {
        if let Some(id) = layer["ind"].as_u64()
            && parents.insert(id, layer["parent"].as_u64()).is_some()
        {
            return Err(invalid("layers.ind", "layer identifiers must be unique"));
        }
    }
    for (&id, &parent) in &parents {
        let mut parent = parent;
        let mut depth = 0;
        while let Some(above) = parent {
            depth += 1;
            if above == id || depth > consts::MAX_DEPTH {
                return Err(invalid(
                    "layers.parent",
                    "cyclic or excessive transform ancestry",
                ));
            }
            parent = *parents
                .get(&above)
                .ok_or_else(|| invalid("layers.parent", "unknown parent layer"))?;
        }
    }
    Ok(())
}

fn nonempty(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        _ => true,
    }
}

fn invalid(path: &str, reason: &'static str) -> ArtworkError {
    ArtworkError::Invalid {
        path: path.to_owned(),
        reason,
    }
}

fn unsupported(path: &str, feature: &'static str) -> ArtworkError {
    ArtworkError::Unsupported {
        path: path.to_owned(),
        feature,
    }
}
