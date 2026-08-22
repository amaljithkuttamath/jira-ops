use std::io::{self, Write};

use serde::Serialize;

use crate::cli::OutputFormat;
use crate::error::AppError;

#[derive(Debug, Serialize)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct NoMeta {}

#[derive(Debug, Serialize)]
pub struct SuccessEnvelope<T, M = NoMeta> {
    pub data: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<M>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
}

impl<T> SuccessEnvelope<T, NoMeta> {
    pub fn new(data: T) -> Self {
        Self {
            data,
            meta: None,
            warnings: Vec::new(),
        }
    }

    pub fn with_meta<M>(data: T, meta: M) -> SuccessEnvelope<T, M> {
        SuccessEnvelope {
            data,
            meta: Some(meta),
            warnings: Vec::new(),
        }
    }
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    error: &'a AppError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorWriteStatus {
    Original,
    InternalFallback,
}

const INTERNAL_TOON_ERROR: &[u8] =
    b"error:\n  code: internal\n  message: failed to write process output\n  retry_safety: safe\n";

pub fn write_success<T: Serialize, M: Serialize>(
    writer: &mut dyn Write,
    value: &SuccessEnvelope<T, M>,
    format: OutputFormat,
    pretty: bool,
) -> io::Result<()> {
    write_document(writer, value, format, pretty)
}

pub fn write_error(
    writer: &mut dyn Write,
    error: &AppError,
    format: OutputFormat,
    pretty: bool,
) -> io::Result<ErrorWriteStatus> {
    match format {
        OutputFormat::Json => {
            write_json(writer, &ErrorEnvelope { error }, pretty)?;
            Ok(ErrorWriteStatus::Original)
        }
        OutputFormat::Toon => match encode_toon(&ErrorEnvelope { error }) {
            Ok(encoded) => {
                write_toon_bytes(writer, &encoded)?;
                Ok(ErrorWriteStatus::Original)
            }
            Err(_) => {
                writer.write_all(INTERNAL_TOON_ERROR)?;
                Ok(ErrorWriteStatus::InternalFallback)
            }
        },
    }
}

fn write_document<T: Serialize>(
    writer: &mut dyn Write,
    value: &T,
    format: OutputFormat,
    pretty: bool,
) -> io::Result<()> {
    match format {
        OutputFormat::Json => write_json(writer, value, pretty),
        OutputFormat::Toon => write_toon(writer, value),
    }
}

fn write_json<T: Serialize + ?Sized>(
    writer: &mut dyn Write,
    value: &T,
    pretty: bool,
) -> io::Result<()> {
    let result = if pretty {
        serde_json::to_writer_pretty(&mut *writer, value)
    } else {
        serde_json::to_writer(&mut *writer, value)
    };
    result.map_err(io::Error::other)?;
    writer.write_all(b"\n")
}

fn write_toon<T: Serialize>(writer: &mut dyn Write, value: &T) -> io::Result<()> {
    let encoded = encode_toon(value)?;
    write_toon_bytes(writer, &encoded)
}

fn encode_toon<T: Serialize>(value: &T) -> io::Result<String> {
    let validation_value = serde_json::to_value(value).map_err(io::Error::other)?;
    validate_toon_controls(&validation_value)?;

    let encoded = toon_format::encode_default(&validation_value).map_err(io::Error::other)?;
    validate_encoded_toon(&encoded)?;
    Ok(encoded)
}

#[cfg(test)]
fn write_encoded_toon(writer: &mut dyn Write, encoded: &str) -> io::Result<()> {
    validate_encoded_toon(encoded)?;
    write_toon_bytes(writer, encoded)
}

fn validate_encoded_toon(encoded: &str) -> io::Result<()> {
    if encoded
        .chars()
        .any(|character| character.is_control() && character != '\n')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TOON encoder emitted an unsupported control character",
        ));
    }
    debug_assert!(!encoded.ends_with('\n'));
    if encoded.ends_with('\n') {
        return Err(io::Error::other(
            "TOON encoder unexpectedly returned a trailing newline",
        ));
    }
    Ok(())
}

fn write_toon_bytes(writer: &mut dyn Write, encoded: &str) -> io::Result<()> {
    writer.write_all(encoded.as_bytes())?;
    writer.write_all(b"\n")
}

fn validate_toon_controls(value: &serde_json::Value) -> io::Result<()> {
    match value {
        serde_json::Value::String(value) => validate_toon_string(value),
        serde_json::Value::Array(values) => {
            for value in values {
                validate_toon_controls(value)?;
            }
            Ok(())
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                validate_toon_string(key)?;
                validate_toon_controls(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_toon_string(value: &str) -> io::Result<()> {
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TOON output contains an unsupported control character",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::{self, Write};

    use serde::{Serialize, Serializer};
    use serde_json::{Value, json};

    use super::{SuccessEnvelope, Warning, write_encoded_toon, write_error, write_success};
    use crate::cli::OutputFormat;
    use crate::error::{AppError, ErrorCode, RetrySafety};

    #[test]
    fn toon_preserves_representative_json_shapes() {
        let data = json!({
            "null": null,
            "bool": true,
            "number": 42.5,
            "quoted": "true",
            "controls": "line one\nline two\tend",
            "unicode": "café 🚀",
            "empty_object": {},
            "empty_array": [],
            "nested": {"child": {"value": "x"}},
            "uniform": [{"id": 1, "name": "one"}, {"id": 2, "name": "two"}],
            "heterogeneous": [1, "two", {"three": 3}]
        });
        let mut envelope = SuccessEnvelope::with_meta(data, json!({"next_cursor": "opaque"}));
        envelope.warnings.push(Warning {
            code: "partial".to_owned(),
            message: "Some fields were omitted".to_owned(),
        });

        let mut output = Vec::new();
        write_success(&mut output, &envelope, OutputFormat::Toon, false).unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
        let decoded: Value = toon_format::decode_default(text.trim_end()).unwrap();
        assert_eq!(decoded, serde_json::to_value(&envelope).unwrap());
    }

    #[test]
    fn toon_error_preserves_the_error_envelope_and_one_lf() {
        let error = AppError::new(
            ErrorCode::InvalidInput,
            "quoted \"message\"\nnext",
            RetrySafety::Safe,
        );
        let expected = json!({
            "error": {
                "code": "invalid_input",
                "message": "quoted \"message\"\nnext",
                "retry_safety": "safe"
            }
        });
        let mut output = Vec::new();
        write_error(&mut output, &error, OutputFormat::Toon, false).unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
        let decoded: Value = toon_format::decode_default(text.trim_end()).unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn json_dynamic_object_preserves_semantics() {
        let envelope = SuccessEnvelope::new(json!({
            "z": {"second": 2, "first": 1},
            "a": [{"delta": 4, "beta": 2}]
        }));
        let mut output = Vec::new();
        write_success(&mut output, &envelope, OutputFormat::Json, false).unwrap();

        let decoded: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(decoded, serde_json::to_value(&envelope).unwrap());
    }

    #[test]
    fn toon_encodes_supported_controls_as_textual_escapes() {
        let envelope = SuccessEnvelope::new(json!({"text": "line\ncarriage\rhorizontal\ttab"}));
        let mut output = Vec::new();
        write_success(&mut output, &envelope, OutputFormat::Toon, false).unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.contains(r"line\ncarriage\rhorizontal\ttab"));
        assert!(!text.contains("line\ncarriage"));
        assert!(!text.contains('\r'));
        assert!(!text.contains('\t'));
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
    }

    #[test]
    fn toon_rejects_unsafe_controls_atomically() {
        for control in [
            '\0', '\u{0001}', '\u{0007}', '\u{000b}', '\u{000c}', '\u{001b}', '\u{007f}',
            '\u{0085}',
        ] {
            let envelope = SuccessEnvelope::new(json!({"text": format!("before{control}after")}));
            let mut output = Vec::new();
            let error = write_success(&mut output, &envelope, OutputFormat::Toon, false)
                .expect_err("unsafe control must be rejected");

            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert!(
                output.is_empty(),
                "partial output for U+{:04X}",
                control as u32
            );
        }
    }

    #[test]
    fn encoded_toon_rejects_raw_controls_atomically() {
        for encoded in [
            "data: before\0after",
            "data: before\tafter",
            "data: before\rafter",
            "data: before\u{001b}after",
            "data: before\u{007f}after",
            "data: before\u{0085}after",
        ] {
            let mut output = Vec::new();
            let error = write_encoded_toon(&mut output, encoded)
                .expect_err("raw encoded control must be rejected");

            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert!(output.is_empty());
        }

        let mut output = Vec::new();
        write_encoded_toon(&mut output, "data:\n  value: safe").unwrap();
        assert_eq!(output, b"data:\n  value: safe\n");
    }

    struct ChangingValue {
        calls: Cell<u8>,
    }

    impl Serialize for ChangingValue {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let call = self.calls.get();
            self.calls.set(call + 1);
            let stage = if call == 0 { "validated" } else { "changed" };
            json!({"stage": stage}).serialize(serializer)
        }
    }

    #[test]
    fn toon_encodes_the_single_json_value_that_passed_preflight() {
        let data = ChangingValue {
            calls: Cell::new(0),
        };
        let envelope = SuccessEnvelope::new(data);
        let mut output = Vec::new();

        write_success(&mut output, &envelope, OutputFormat::Toon, false).unwrap();

        assert_eq!(envelope.data.calls.get(), 1);
        let document = std::str::from_utf8(&output)
            .unwrap()
            .strip_suffix('\n')
            .unwrap();
        let decoded: Value = toon_format::decode_default(document).unwrap();
        assert_eq!(decoded, json!({"data":{"stage":"validated"}}));
    }

    fn rejected_controls() -> impl Iterator<Item = char> {
        (0..=0x9f)
            .filter_map(char::from_u32)
            .filter(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    }

    fn object_with_key(key: String, value: Value) -> Value {
        let mut object = serde_json::Map::new();
        object.insert(key, value);
        Value::Object(object)
    }

    fn unsafe_success_positions(control: char) -> Vec<(&'static str, Value)> {
        let text = format!("before{control}after");
        vec![
            ("root key", object_with_key(text.clone(), Value::Null)),
            (
                "nested key",
                json!({"outer": object_with_key(text.clone(), Value::Bool(true))}),
            ),
            ("root value", Value::String(text.clone())),
            ("nested value", json!({"outer":{"inner":text.clone()}})),
            ("primitive array cell", json!([1, text.clone(), true])),
            (
                "tabular header",
                Value::Array(vec![
                    object_with_key(text.clone(), json!(1)),
                    object_with_key(text.clone(), json!(2)),
                ]),
            ),
            (
                "tabular cell",
                json!([{"field":text.clone()},{"field":"safe"}]),
            ),
            (
                "mutation echo",
                json!({
                    "operation":"issue.comment",
                    "intent":{"issue":"DEMO-1","body":text}
                }),
            ),
        ]
    }

    #[test]
    fn toon_rejects_every_unsupported_control_across_reachable_success_positions() {
        for control in rejected_controls() {
            for (position, data) in unsafe_success_positions(control) {
                let envelope = SuccessEnvelope::new(data);
                let mut output = Vec::new();
                let error = write_success(&mut output, &envelope, OutputFormat::Toon, false)
                    .expect_err("unsupported control must fail before output");
                assert_eq!(
                    error.kind(),
                    io::ErrorKind::InvalidData,
                    "U+{:04X} at {position}",
                    control as u32
                );
                assert!(
                    output.is_empty(),
                    "partial output for U+{:04X} at {position}",
                    control as u32
                );
            }

            for (position, warning) in [
                (
                    "warning code",
                    Warning {
                        code: format!("before{control}after"),
                        message: "safe".to_owned(),
                    },
                ),
                (
                    "warning message",
                    Warning {
                        code: "safe".to_owned(),
                        message: format!("before{control}after"),
                    },
                ),
            ] {
                let mut envelope = SuccessEnvelope::new(json!({"ok":true}));
                envelope.warnings.push(warning);
                let mut output = Vec::new();
                write_success(&mut output, &envelope, OutputFormat::Toon, false)
                    .expect_err("unsafe warning must fail before output");
                assert!(
                    output.is_empty(),
                    "partial output for U+{:04X} at {position}",
                    control as u32
                );
            }
        }
    }

    #[test]
    fn unsafe_error_details_use_the_closed_internal_toon_fallback() {
        const INTERNAL_ERROR: &str = "error:\n  code: internal\n  message: failed to write process output\n  retry_safety: safe\n";

        for control in rejected_controls() {
            let mut error = AppError::new(
                ErrorCode::InvalidInput,
                "safe original message",
                RetrySafety::Safe,
            );
            error.details = Some(json!({
                "nested": {"value": format!("before{control}after")}
            }));
            let mut output = Vec::new();

            write_error(&mut output, &error, OutputFormat::Toon, false)
                .expect("unsafe original error must use the safe fallback");

            assert_eq!(
                std::str::from_utf8(&output).unwrap(),
                INTERNAL_ERROR,
                "U+{:04X}",
                control as u32
            );
        }
    }

    #[test]
    fn toon_escapes_supported_controls_across_envelope_positions() {
        let controls = "before\n\r\tafter";
        let mut envelope = SuccessEnvelope::new(json!({
            "nested": object_with_key(controls.to_owned(), json!([controls])),
            "tabular": [{"field":controls},{"field":"safe"}],
            "mutation": {"operation":"issue.comment","body":controls}
        }));
        envelope.warnings.push(Warning {
            code: "supported_controls".to_owned(),
            message: controls.to_owned(),
        });
        let mut output = Vec::new();

        write_success(&mut output, &envelope, OutputFormat::Toon, false).unwrap();

        let document = std::str::from_utf8(&output)
            .unwrap()
            .strip_suffix('\n')
            .unwrap();
        assert!(!document.contains('\r'));
        assert!(!document.contains('\t'));
        assert!(!document.contains("before\n"));
        assert!(document.contains(r"before\n\r\tafter"));
        let decoded: Value = toon_format::decode_default(document).unwrap();
        assert_eq!(
            decoded,
            json!({
                "data": {
                    "nested": object_with_key(controls.to_owned(), json!([controls])),
                    "tabular": [{"field":controls},{"field":"safe"}],
                    "mutation": {"operation":"issue.comment","body":controls}
                },
                "warnings": [{
                    "code":"supported_controls",
                    "message":controls
                }]
            })
        );

        let mut error = AppError::new(ErrorCode::InvalidInput, controls, RetrySafety::Safe);
        error.details = Some(object_with_key(controls.to_owned(), json!([controls])));
        let mut error_output = Vec::new();
        write_error(&mut error_output, &error, OutputFormat::Toon, false).unwrap();
        let error_document = std::str::from_utf8(&error_output)
            .unwrap()
            .strip_suffix('\n')
            .unwrap();
        assert!(!error_document.contains('\r'));
        assert!(!error_document.contains('\t'));
        let decoded_error: Value = toon_format::decode_default(error_document).unwrap();
        assert_eq!(
            decoded_error,
            json!({
                "error": {
                    "code":"invalid_input",
                    "message":controls,
                    "retry_safety":"safe",
                    "details":object_with_key(controls.to_owned(), json!([controls]))
                }
            })
        );
    }

    #[test]
    fn encoded_toon_rejects_every_raw_unsupported_control_before_write() {
        for control in rejected_controls() {
            let encoded = format!("data: before{control}after");
            let mut output = Vec::new();

            let error = write_encoded_toon(&mut output, &encoded)
                .expect_err("raw unsupported control must be contained");

            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(output.is_empty(), "raw U+{:04X}", control as u32);
        }
    }

    struct ShortWriter {
        bytes: Vec<u8>,
        maximum_chunk: usize,
    }

    impl Write for ShortWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let accepted = buffer.len().min(self.maximum_chunk);
            self.bytes.extend_from_slice(&buffer[..accepted]);
            Ok(accepted)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct PrefixFailWriter {
        bytes: Vec<u8>,
        prefix_limit: usize,
        calls: usize,
    }

    impl Write for PrefixFailWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.bytes.len() >= self.prefix_limit {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "writer failed"));
            }
            let accepted = buffer
                .len()
                .min(self.prefix_limit.saturating_sub(self.bytes.len()));
            self.bytes.extend_from_slice(&buffer[..accepted]);
            Ok(accepted)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn toon_write_all_completes_deterministic_short_writes() {
        const EXPECTED: &[u8] =
            b"error:\n  code: invalid_input\n  message: failure\n  retry_safety: safe\n";
        let error = AppError::new(ErrorCode::InvalidInput, "failure", RetrySafety::Safe);
        let mut writer = ShortWriter {
            bytes: Vec::new(),
            maximum_chunk: 3,
        };

        write_error(&mut writer, &error, OutputFormat::Toon, false).unwrap();

        assert_eq!(writer.bytes, EXPECTED);
    }

    #[test]
    fn toon_writer_failure_after_prefix_never_appends_a_fallback() {
        let error = AppError::new(ErrorCode::InvalidInput, "failure", RetrySafety::Safe);
        let mut writer = PrefixFailWriter {
            bytes: Vec::new(),
            prefix_limit: 7,
            calls: 0,
        };

        let write_error = write_error(&mut writer, &error, OutputFormat::Toon, false)
            .expect_err("writer failure must propagate");

        assert_eq!(write_error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(writer.bytes, b"error:\n");
        assert_eq!(writer.calls, 2);
    }

    #[test]
    fn toon_writer_failure_before_first_byte_never_retries() {
        let error = AppError::new(ErrorCode::InvalidInput, "failure", RetrySafety::Safe);
        let mut writer = PrefixFailWriter {
            bytes: Vec::new(),
            prefix_limit: 0,
            calls: 0,
        };

        write_error(&mut writer, &error, OutputFormat::Toon, false)
            .expect_err("writer failure must propagate");

        assert!(writer.bytes.is_empty());
        assert_eq!(writer.calls, 1);
    }
}
