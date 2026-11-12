//! Bounded decoding for both Tauri custom-protocol and postMessage imports.

use base64::Engine;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportMetadata {
    pub(crate) name: String,
    pub(crate) source_uri: Option<String>,
    pub(crate) request_id: Option<String>,
}

fn allocate(length: usize, limit: usize) -> Result<Vec<u8>, String> {
    if length == 0 || length > limit {
        return Err("Choose a nonempty document no larger than 32 MiB.".into());
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length)
        .map_err(|_| "Not enough memory to import this photo. Try a smaller capture.")?;
    Ok(bytes)
}

pub(crate) fn copy_raw(raw: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = allocate(raw.len(), limit)?;
    bytes.extend_from_slice(raw);
    Ok(bytes)
}

pub(crate) fn decode_json(value: &Value, limit: usize) -> Result<Vec<u8>, String> {
    if let Some(object) = value.as_object() {
        if object.len() != 1 { return Err("Photo import requires a binary byte payload.".into()); }
        let encoded = object.get("base64").and_then(Value::as_str)
            .ok_or("Photo import requires a binary byte payload.")?;
        // Bound the transport string and decoded size before the decoder can
        // allocate. Standard padded base64 uses four characters per three bytes.
        let max_encoded = limit.div_ceil(3).checked_mul(4).ok_or("Photo import is too large.")?;
        if encoded.is_empty() || encoded.len() > max_encoded || encoded.len() % 4 != 0 {
            return Err("Choose a nonempty document no larger than 32 MiB encoded as base64.".into());
        }
        let padding = encoded.as_bytes().iter().rev().take_while(|&&b| b == b'=').count();
        if padding > 2 { return Err("Photo import contains invalid base64.".into()); }
        let decoded_length = encoded.len() / 4 * 3 - padding;
        if decoded_length == 0 || decoded_length > limit {
            return Err("Choose a nonempty document no larger than 32 MiB.".into());
        }
        // Two padding bytes are the maximum difference between the decoder's
        // capacity estimate and the validated actual length. Reserve fallibly.
        let capacity = encoded.len() / 4 * 3;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity).map_err(|_| "Not enough memory to decode this image.")?;
        bytes.resize(capacity, 0);
        let actual = base64::engine::general_purpose::STANDARD.decode_slice(encoded, &mut bytes)
            .map_err(|_| "Photo import contains invalid base64.")?;
        bytes.truncate(actual);
        return Ok(bytes);
    }
    // Only the exact byte-array shape produced by Tauri is accepted. Check the
    // length before allocating and never truncate or coerce JSON values to u8.
    let values = value.as_array().ok_or("Photo import requires a binary byte payload.")?;
    // Compatibility only: old clients expand bytes into serde JSON values before
    // this boundary. New Android clients must use base64 for larger documents.
    if values.len() > 1024 * 1024 {
        return Err("Legacy JSON byte imports are limited to 1 MiB. Use the base64 transport for documents up to 32 MiB.".into());
    }
    let mut bytes = allocate(values.len(), limit)?;
    for value in values {
        let byte = value.as_u64().and_then(|n| u8::try_from(n).ok())
            .ok_or("Photo import contains an invalid byte.")?;
        bytes.push(byte);
    }
    Ok(bytes)
}

pub(crate) fn decode_metadata(header: Option<&[u8]>) -> Result<ImportMetadata, String> {
    let header = header.ok_or("Missing photo import metadata.")?;
    if header.len() > 16 * 1024 { return Err("Photo import metadata is too large.".into()); }
    let json = base64::engine::general_purpose::STANDARD.decode(header)
        .map_err(|_| "Photo import metadata is invalid.")?;
    serde_json::from_slice(&json).map_err(|_| "Photo import metadata is invalid.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn raw_and_post_message_preserve_all_byte_values() {
        let raw: Vec<u8> = (0..=255).collect();
        assert_eq!(copy_raw(&raw, 256).unwrap(), raw);
        assert_eq!(decode_json(&json!(raw), 256).unwrap(), raw);
    }

    #[test]
    fn both_transports_enforce_nonempty_and_limit() {
        for raw in [vec![], vec![0; 4]] {
            assert!(copy_raw(&raw, 3).unwrap_err().contains("32 MiB"));
            assert!(decode_json(&json!(raw), 3).unwrap_err().contains("32 MiB"));
        }
        // An oversized array is rejected by length before inspecting its values.
        assert!(decode_json(&json!([null, null, null, null]), 3).unwrap_err().contains("32 MiB"));
        assert_eq!(decode_json(&json!([0, 128, 255]), 3).unwrap(), [0, 128, 255]);
    }

    #[test]
    fn rejects_non_arrays_and_non_byte_elements() {
        for value in [json!(null), json!({"0": 12}), json!("AA=="), json!(12)] {
            assert!(decode_json(&value, 10).is_err());
        }
        for value in [json!(-1), json!(256), json!(1.5), json!(1.0), json!(true), json!("1"), json!(null), json!([]), json!({})] {
            assert!(decode_json(&json!([0, value]), 10).is_err());
        }
    }

    #[test]
    fn post_message_metadata_preserves_unicode_provider_and_request_identity() {
        let original = json!({"name": "Überblick-日本語-📷.pdf", "sourceUri": "content://documents/日本語%3APDF", "requestId": "import-test"});
        let encoded = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&original).unwrap());
        // Tauri's postMessage envelope carries options.headers unchanged.
        let envelope = json!({"payload": [0, 128, 255], "options": {"headers": {"x-ocr-metadata": encoded}}});
        let received = decode_metadata(Some(envelope["options"]["headers"]["x-ocr-metadata"].as_str().unwrap().as_bytes())).unwrap();
        assert_eq!(received, ImportMetadata { name: original["name"].as_str().unwrap().into(), source_uri: Some(original["sourceUri"].as_str().unwrap().into()), request_id: Some("import-test".into()) });
        assert_eq!(decode_json(&envelope["payload"], 3).unwrap(), [0, 128, 255]);
        let minimal = base64::engine::general_purpose::STANDARD.encode(br#"{"name":"photo.png"}"#);
        let metadata = decode_metadata(Some(minimal.as_bytes())).unwrap();
        assert_eq!((metadata.source_uri, metadata.request_id), (None, None));
    }

    #[test]
    fn metadata_retains_missing_size_and_encoding_checks() {
        assert!(decode_metadata(None).unwrap_err().contains("Missing"));
        assert!(decode_metadata(Some(&vec![b'A'; 16 * 1024 + 1])).unwrap_err().contains("too large"));
        for header in [b"!".as_slice(), b"e30=", b"/w=="] {
            assert!(decode_metadata(Some(header)).unwrap_err().contains("invalid"));
        }
    }

    #[test]
    fn base64_preserves_bytes_and_enforces_encoded_and_decoded_limits() {
        for length in [1, 2, 3, 255, 256, 257] {
            let bytes: Vec<u8> = (0..length).map(|n| n as u8).collect();
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            assert_eq!(decode_json(&json!({"base64": encoded}), length).unwrap(), bytes);
            assert!(decode_json(&json!({"base64": encoded}), length - 1).is_err());
        }
        // Equal encoded lengths can represent distinct decoded sizes.
        assert_eq!(decode_json(&json!({"base64": "AA=="}), 1).unwrap(), [0]);
        assert!(decode_json(&json!({"base64": "AAA="}), 1).is_err());
        assert!(decode_json(&json!({"base64": "AAAA"}), 1).is_err());
        for value in [json!({"base64": ""}), json!({"base64": "A==="}), json!({"base64": "AA"}), json!({"base64": "!!!!"}), json!({"base64": "AB=="}), json!({"base64": 1}), json!({"base64": "AA==", "extra": true})] {
            assert!(decode_json(&value, 32).is_err());
        }
    }

    #[test]
    fn legacy_arrays_have_an_explicit_one_mib_compatibility_limit() {
        let value = Value::Array(vec![Value::Null; 1024 * 1024 + 1]);
        assert!(decode_json(&value, 32 * 1024 * 1024).unwrap_err().contains("1 MiB"));
    }
}
