//! The parts of Core's `UniValue` that dash-cli uses beyond what dash-args ports.
//!
//! The value type, reader and writer are `dash_args::univalue::SettingsValue` (Core's
//! `UniValue`, `univalue_read.cpp` and `univalue_write.cpp`). This module adds the lookups and
//! getters of `univalue.cpp` and `univalue_get.cpp` with Core's semantics: the `operator[]`
//! overloads return `NullUniValue` rather than failing, and the strict getters throw
//! `std::runtime_error` texts that dash-cli prints.

pub use dash_args::univalue::{RuntimeError, SettingsValue as UniValue, ValueType};

/// Core: `NullUniValue`.
pub static NULL: UniValue = UniValue::Null;

fn expect(value: &UniValue, expected: ValueType) -> Result<(), RuntimeError> {
    if value.value_type() == expected {
        Ok(())
    } else {
        Err(RuntimeError(format!(
            "JSON value of type {} is not of expected type {}",
            value.value_type().name(),
            expected.name()
        )))
    }
}

/// Core: the `UniValue` members dash-cli calls.
pub trait UniValueExt {
    /// Core: `operator[](const std::string&)`: the first value under `key` of an object, else null.
    fn key(&self, key: &str) -> &UniValue;
    /// Core: `operator[](size_t)`: element `index` of an array or object, else null.
    fn at(&self, index: usize) -> &UniValue;
    /// Core: `find_value`: the first value under `key` of an object, else null.
    fn find_value(&self, key: &str) -> &UniValue;
    /// Core: `size()`.
    fn size(&self) -> usize;
    /// Core: `empty()`.
    fn is_empty_container(&self) -> bool;
    /// Core: `getKeys()`.
    fn get_keys(&self) -> Result<Vec<&str>, RuntimeError>;
    /// Core: `getInt<int>()`.
    fn get_int(&self) -> Result<i32, RuntimeError>;
    /// Core: `get_real()`.
    fn get_real(&self) -> Result<f64, RuntimeError>;
    /// Core: `get_obj()`.
    fn get_obj(&self) -> Result<&UniValue, RuntimeError>;
    /// Core: `pushKV` on an object: replaces the first value under the key, else appends.
    fn push_kv(&mut self, key: &str, value: UniValue);
}

impl UniValueExt for UniValue {
    fn key(&self, key: &str) -> &UniValue {
        self.find_value(key)
    }

    fn at(&self, index: usize) -> &UniValue {
        match self {
            UniValue::Arr(values) => values.get(index).unwrap_or(&NULL),
            UniValue::Obj(entries) => entries.get(index).map_or(&NULL, |(_, v)| v),
            _ => &NULL,
        }
    }

    fn find_value(&self, key: &str) -> &UniValue {
        match self {
            UniValue::Obj(entries) => {
                entries.iter().find(|(k, _)| k == key).map_or(&NULL, |(_, v)| v)
            }
            _ => &NULL,
        }
    }

    fn size(&self) -> usize {
        match self {
            UniValue::Arr(values) => values.len(),
            UniValue::Obj(entries) => entries.len(),
            _ => 0,
        }
    }

    fn is_empty_container(&self) -> bool {
        self.size() == 0
    }

    fn get_keys(&self) -> Result<Vec<&str>, RuntimeError> {
        expect(self, ValueType::Obj)?;
        match self {
            UniValue::Obj(entries) => Ok(entries.iter().map(|(k, _)| k.as_str()).collect()),
            _ => unreachable!("checked above"),
        }
    }

    fn get_int(&self) -> Result<i32, RuntimeError> {
        // `getInt<int>` is `std::from_chars` over the whole text, like `getInt<int64_t>`.
        let value = self.get_int64()?;
        i32::try_from(value).map_err(|_| RuntimeError("JSON integer out of range".to_string()))
    }

    fn get_real(&self) -> Result<f64, RuntimeError> {
        expect(self, ValueType::Num)?;
        // Core reads the text with an `istringstream`, which fails on a value out of the
        // double range; the text is a JSON number, so only an infinite result is an error.
        match self.get_val_str().parse::<f64>() {
            Ok(value) if value.is_finite() => Ok(value),
            _ => Err(RuntimeError("JSON double out of range".to_string())),
        }
    }

    fn get_obj(&self) -> Result<&UniValue, RuntimeError> {
        expect(self, ValueType::Obj)?;
        Ok(self)
    }

    fn push_kv(&mut self, key: &str, value: UniValue) {
        match self {
            UniValue::Obj(entries) => match entries.iter_mut().find(|(k, _)| k == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key.to_string(), value)),
            },
            other => panic!("pushKV on a {} value", other.value_type().name()),
        }
    }
}

/// Core: `JSONRPCRequestObj`.
pub fn jsonrpc_request_obj(method: &str, params: UniValue, id: i32) -> UniValue {
    let mut request = UniValue::new_object();
    request.push_kv("method", method.into());
    request.push_kv("params", params);
    request.push_kv("id", id.into());
    request
}

/// Core: `JSONRPCReplyObj(result, NullUniValue, id)`, the only form dash-cli builds.
pub fn jsonrpc_reply_obj(result: UniValue, id: i32) -> UniValue {
    let mut reply = UniValue::new_object();
    reply.push_kv("result", result);
    reply.push_kv("error", UniValue::Null);
    reply.push_kv("id", id.into());
    reply
}

/// Core: `JSONRPCProcessBatchReply`.
pub fn jsonrpc_process_batch_reply(batch: &UniValue) -> Result<Vec<UniValue>, RuntimeError> {
    let UniValue::Arr(records) = batch else {
        return Err(RuntimeError("Batch must be an array".to_string()));
    };
    let mut out = vec![UniValue::Null; records.len()];
    for record in records {
        if !record.is_object() {
            return Err(RuntimeError("Batch member must be an object".to_string()));
        }
        // `size_t id = rec["id"].getInt<int>()`: a negative id wraps to a huge size_t.
        let id = record.key("id").get_int()? as isize as usize;
        if id >= records.len() {
            return Err(RuntimeError("Batch member id is larger than batch size".to_string()));
        }
        out[id] = record.clone();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> UniValue {
        UniValue::read(text.as_bytes()).expect("valid JSON")
    }

    #[test]
    fn lookups_return_null_on_mismatch() {
        let v = parse(r#"{"a":[1,2],"b":null}"#);
        assert!(v.key("missing").is_null());
        assert!(v.key("a").at(5).is_null());
        assert!(parse("[1]").key("a").is_null());
        assert_eq!(v.key("a").at(1).get_int(), Ok(2));
    }

    #[test]
    fn strict_getters_use_cores_texts() {
        assert_eq!(
            UniValue::Null.get_int().unwrap_err().0,
            "JSON value of type null is not of expected type number"
        );
        assert_eq!(parse("1.5").get_int().unwrap_err().0, "JSON integer out of range");
        assert_eq!(parse("3000000000").get_int().unwrap_err().0, "JSON integer out of range");
        assert_eq!(parse("1e999").get_real().unwrap_err().0, "JSON double out of range");
        assert_eq!(parse("0.25").get_real(), Ok(0.25));
    }

    #[test]
    fn push_kv_replaces_the_first_value() {
        let mut o = UniValue::new_object();
        o.push_kv("a", 1.into());
        o.push_kv("b", 2.into());
        o.push_kv("a", 3.into());
        assert_eq!(o.write(), r#"{"a":3,"b":2}"#);
    }

    #[test]
    fn batch_reply_orders_by_id() {
        let batch = parse(r#"[{"id":1,"result":"b"},{"id":0,"result":"a"}]"#);
        let out = jsonrpc_process_batch_reply(&batch).unwrap();
        assert_eq!(out[0].key("result").get_str(), Ok("a"));
        assert_eq!(out[1].key("result").get_str(), Ok("b"));
        assert_eq!(
            jsonrpc_process_batch_reply(&parse(r#"[{"id":2}]"#)).unwrap_err().0,
            "Batch member id is larger than batch size"
        );
        assert_eq!(
            jsonrpc_process_batch_reply(&parse("{}")).unwrap_err().0,
            "Batch must be an array"
        );
    }
}
