use serde_json::{Value, json};

use crate::error::{AppError, ErrorCode, RetrySafety};

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_DEPTH: usize = 128;
const MAX_GENERATED_PARAGRAPHS: usize = 10_000;
const MAX_GENERATED_ADF_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Default)]
struct Context {
    list_depth: usize,
    continuation: String,
}

enum Event<'a> {
    Node {
        node: &'a Value,
        depth: usize,
        context: Context,
    },
    ListItem {
        node: &'a Value,
        depth: usize,
        list_depth: usize,
        prefix: String,
    },
    Literal(String),
    Newline,
}

pub fn adf_to_text(value: &Value) -> Result<String, AppError> {
    if node_type(value)? != "doc" {
        return Err(invalid_adf());
    }
    let mut output = String::new();
    let mut stack = vec![Event::Node {
        node: value,
        depth: 0,
        context: Context::default(),
    }];

    while let Some(event) = stack.pop() {
        match event {
            Event::Literal(text) => append_bounded(&mut output, &text)?,
            Event::Newline => append_bounded(&mut output, "\n")?,
            Event::ListItem {
                node,
                depth,
                list_depth,
                prefix,
            } => push_list_item(&mut stack, &mut output, node, depth, list_depth, prefix)?,
            Event::Node {
                node,
                depth,
                context,
            } => {
                if depth > MAX_DEPTH {
                    return Err(invalid_adf());
                }
                render_node(&mut stack, &mut output, node, depth, context)?;
            }
        }
    }

    Ok(output)
}

pub fn text_to_adf(text: &str) -> Value {
    build_text_adf(text)
}

pub fn checked_text_to_adf(text: &str) -> Result<Value, AppError> {
    if paragraph_count(text) > MAX_GENERATED_PARAGRAPHS {
        return Err(generated_adf_too_large());
    }
    let adf = build_text_adf(text);
    if serde_json::to_vec(&adf)
        .map_err(|_| generated_adf_too_large())?
        .len()
        > MAX_GENERATED_ADF_BYTES
    {
        return Err(generated_adf_too_large());
    }
    Ok(adf)
}

fn build_text_adf(text: &str) -> Value {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let content = normalized
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                json!({"type":"paragraph","content":[]})
            } else {
                json!({"type":"paragraph","content":[{"type":"text","text":line}]})
            }
        })
        .collect::<Vec<_>>();
    json!({"type":"doc","version":1,"content":content})
}

fn paragraph_count(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut paragraphs = 1_usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                paragraphs = paragraphs.saturating_add(1);
                if bytes.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
            }
            b'\n' => paragraphs = paragraphs.saturating_add(1),
            _ => {}
        }
        index += 1;
    }
    paragraphs
}

fn generated_adf_too_large() -> AppError {
    let mut error = AppError::new(
        ErrorCode::SchemaViolation,
        "plain text generates an ADF document above the planning limit",
        RetrySafety::Safe,
    );
    error.operation_outcome = Some(crate::error::OperationOutcome::NotApplied);
    error
}

fn render_node<'a>(
    stack: &mut Vec<Event<'a>>,
    output: &mut String,
    node: &'a Value,
    depth: usize,
    context: Context,
) -> Result<(), AppError> {
    let kind = node_type(node)?;
    match kind {
        "doc" => push_children(stack, content(node)?, depth, context, true),
        "paragraph" | "heading" | "codeBlock" => {
            push_children(stack, content(node)?, depth, context, false)
        }
        "blockquote" | "panel" | "table" | "tableRow" | "tableCell" | "tableHeader" => {
            push_children(stack, content(node)?, depth, context, true)
        }
        "text" => append_bounded(output, &render_text(node)?),
        "hardBreak" => {
            append_bounded(output, "\n")?;
            append_bounded(output, &context.continuation)
        }
        "mention" => {
            let visible = node
                .get("attrs")
                .and_then(Value::as_object)
                .and_then(|attrs| {
                    attrs
                        .get("text")
                        .or_else(|| attrs.get("displayName"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("[mention]");
            append_bounded(output, visible)
        }
        "media" | "mediaSingle" | "mediaGroup" => append_bounded(output, "[media]"),
        "inlineCard" => append_bounded(output, "[card]"),
        "bulletList" | "orderList" => {
            push_list(stack, node, depth, context.list_depth, kind == "orderList")
        }
        "listItem" => push_children(stack, content(node)?, depth, context, true),
        _ => match content_optional(node)? {
            Some(children) if !children.is_empty() => {
                push_children(stack, children, depth, context, true)
            }
            _ => append_bounded(
                output,
                &format!("[unsupported:{}]", bounded_type_name(kind)),
            ),
        },
    }
}

fn push_children<'a>(
    stack: &mut Vec<Event<'a>>,
    children: &'a [Value],
    depth: usize,
    context: Context,
    separated: bool,
) -> Result<(), AppError> {
    if depth >= MAX_DEPTH && !children.is_empty() {
        return Err(invalid_adf());
    }
    for index in (0..children.len()).rev() {
        stack.push(Event::Node {
            node: &children[index],
            depth: depth + 1,
            context: context.clone(),
        });
        if separated && index > 0 {
            stack.push(Event::Newline);
        }
    }
    Ok(())
}

fn push_list<'a>(
    stack: &mut Vec<Event<'a>>,
    node: &'a Value,
    depth: usize,
    list_depth: usize,
    ordered: bool,
) -> Result<(), AppError> {
    let children = content(node)?;
    let start = if ordered {
        node.get("attrs")
            .and_then(Value::as_object)
            .and_then(|attrs| attrs.get("order"))
            .and_then(Value::as_u64)
            .unwrap_or(1)
    } else {
        1
    };
    for index in (0..children.len()).rev() {
        if node_type(&children[index])? != "listItem" {
            return Err(invalid_adf());
        }
        let prefix = if ordered {
            format!("{}. ", start.saturating_add(index as u64))
        } else {
            "- ".to_owned()
        };
        stack.push(Event::ListItem {
            node: &children[index],
            depth: depth + 1,
            list_depth,
            prefix,
        });
        if index > 0 {
            stack.push(Event::Newline);
        }
    }
    Ok(())
}

fn push_list_item<'a>(
    stack: &mut Vec<Event<'a>>,
    output: &mut String,
    node: &'a Value,
    depth: usize,
    list_depth: usize,
    prefix: String,
) -> Result<(), AppError> {
    if depth > MAX_DEPTH || node_type(node)? != "listItem" {
        return Err(invalid_adf());
    }
    let indent = "  ".repeat(list_depth);
    append_bounded(output, &indent)?;
    append_bounded(output, &prefix)?;
    let continuation = " ".repeat(indent.len() + prefix.len());
    let children = content(node)?;
    let mut sequence = Vec::new();
    for (index, child) in children.iter().enumerate() {
        let child_type = node_type(child)?;
        let is_list = matches!(child_type, "bulletList" | "orderList");
        if index > 0 || is_list {
            sequence.push(Event::Newline);
            if !is_list {
                sequence.push(Event::Literal(continuation.clone()));
            }
        }
        sequence.push(Event::Node {
            node: child,
            depth: depth + 1,
            context: Context {
                list_depth: if is_list { list_depth + 1 } else { list_depth },
                continuation: continuation.clone(),
            },
        });
    }
    stack.extend(sequence.into_iter().rev());
    Ok(())
}

fn render_text(node: &Value) -> Result<String, AppError> {
    let text = node
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(invalid_adf)?;
    let href = node
        .get("marks")
        .and_then(Value::as_array)
        .and_then(|marks| {
            marks.iter().find_map(|mark| {
                (mark.get("type").and_then(Value::as_str) == Some("link"))
                    .then(|| {
                        mark.get("attrs")
                            .and_then(Value::as_object)
                            .and_then(|attrs| attrs.get("href"))
                            .and_then(Value::as_str)
                    })
                    .flatten()
            })
        });
    if let Some(href) = href
        && href != text
    {
        return Ok(format!("{text} ({href})"));
    }
    Ok(text.to_owned())
}

fn node_type(node: &Value) -> Result<&str, AppError> {
    node.as_object()
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str)
        .ok_or_else(invalid_adf)
}

fn content(node: &Value) -> Result<&[Value], AppError> {
    content_optional(node)?.ok_or_else(invalid_adf)
}

fn content_optional(node: &Value) -> Result<Option<&[Value]>, AppError> {
    match node.get("content") {
        None => Ok(None),
        Some(Value::Array(children)) => Ok(Some(children)),
        Some(_) => Err(invalid_adf()),
    }
}

fn bounded_type_name(kind: &str) -> String {
    let name: String = kind
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(32)
        .collect();
    if name.is_empty() {
        "unknown".to_owned()
    } else {
        name
    }
}

fn append_bounded(output: &mut String, text: &str) -> Result<(), AppError> {
    let length = output
        .len()
        .checked_add(text.len())
        .ok_or_else(projected_text_too_large)?;
    ensure_projected_text_length(length)?;
    output.push_str(text);
    Ok(())
}

pub fn ensure_projected_text_within_limit(text: &str) -> Result<(), AppError> {
    ensure_projected_text_length(text.len())
}

fn ensure_projected_text_length(length: usize) -> Result<(), AppError> {
    if length > MAX_OUTPUT_BYTES {
        return Err(projected_text_too_large());
    }
    Ok(())
}

fn projected_text_too_large() -> AppError {
    AppError::new(
        ErrorCode::ResponseTooLarge,
        "ADF plain text exceeded the 1 MiB limit",
        RetrySafety::Safe,
    )
}

fn invalid_adf() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned malformed ADF",
        RetrySafety::Safe,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{adf_to_text, text_to_adf};
    use crate::error::ErrorCode;

    #[test]
    fn nested_lists_have_exact_prefixes() {
        let adf = fixture(include_str!("../tests/fixtures/adf/nested-lists.json"));
        assert_eq!(adf_to_text(&adf).unwrap(), "- first\n  1. nested");
    }

    #[test]
    fn rich_inline_nodes_have_deterministic_text() {
        let adf = fixture(include_str!("../tests/fixtures/adf/rich-inline.json"));
        assert_eq!(
            adf_to_text(&adf).unwrap(),
            "Atlassian (https://atlassian.com) by @Agent\ndone"
        );
    }

    #[test]
    fn unknown_parent_keeps_children_and_unknown_leaves_are_bounded() {
        let adf = fixture(include_str!("../tests/fixtures/adf/unsupported-nodes.json"));
        assert_eq!(
            adf_to_text(&adf).unwrap(),
            "kept\n[unsupported:future-node]\n[media]\n[card]"
        );
        let leaf = json!({"type":"doc","content":[{"type":"!!!very-long-invalid-node-name-that-must-not-leak!!!"}]});
        assert_eq!(
            adf_to_text(&leaf).unwrap(),
            "[unsupported:very-long-invalid-node-name-that]"
        );
    }

    #[test]
    fn empty_paragraphs_and_trailing_empty_paragraph_are_preserved() {
        let adf = json!({
            "type":"doc",
            "version":1,
            "content":[
                {"type":"paragraph","content":[{"type":"text","text":"a"}]},
                {"type":"paragraph","content":[]},
                {"type":"paragraph","content":[{"type":"text","text":"b"}]},
                {"type":"paragraph","content":[]}
            ]
        });
        assert_eq!(adf_to_text(&adf).unwrap(), "a\n\nb\n");
    }

    #[test]
    fn plain_text_normalizes_line_endings_without_markdown_inference() {
        assert_eq!(
            text_to_adf("# heading\r\n\rline\n"),
            json!({
                "type":"doc",
                "version":1,
                "content":[
                    {"type":"paragraph","content":[{"type":"text","text":"# heading"}]},
                    {"type":"paragraph","content":[]},
                    {"type":"paragraph","content":[{"type":"text","text":"line"}]},
                    {"type":"paragraph","content":[]}
                ]
            })
        );
    }

    #[test]
    fn projected_field_larger_than_one_mib_is_rejected_not_truncated() {
        let adf = json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"x".repeat(1024 * 1024 + 1)}]}]});
        let error = adf_to_text(&adf).unwrap_err();
        assert_eq!(error.code, ErrorCode::ResponseTooLarge);
    }

    #[test]
    fn ordered_lists_honor_the_start_number() {
        let adf = json!({
            "type":"doc",
            "content":[{
                "type":"orderList",
                "attrs":{"order":4},
                "content":[
                    {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"four"}]}]},
                    {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"five"}]}]}
                ]
            }]
        });
        assert_eq!(adf_to_text(&adf).unwrap(), "4. four\n5. five");
    }

    #[test]
    fn placeholders_do_not_expose_attributes() {
        let adf = json!({
            "type":"doc",
            "content":[
                {"type":"mention","attrs":{"id":"private-account-id"}},
                {"type":"media","attrs":{"id":"private-media-id"}},
                {"type":"inlineCard","attrs":{"url":"https://private.invalid/path"}},
                {"type":"!!!"}
            ]
        });
        assert_eq!(
            adf_to_text(&adf).unwrap(),
            "[mention]\n[media]\n[card]\n[unsupported:unknown]"
        );
    }

    #[test]
    fn malformed_or_excessively_deep_adf_is_rejected() {
        let malformed = json!({"type":"doc","content":"not-an-array"});
        assert_eq!(
            adf_to_text(&malformed).unwrap_err().code,
            ErrorCode::ResponseInvalid
        );

        let mut node = json!({"type":"text","text":"bottom"});
        for _ in 0..129 {
            node = json!({"type":"future-parent","content":[node]});
        }
        let deep = json!({"type":"doc","content":[node]});
        assert_eq!(
            adf_to_text(&deep).unwrap_err().code,
            ErrorCode::ResponseInvalid
        );
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_json_never_panics(source in ".{0,512}") {
            if let Ok(value) = serde_json::from_str::<Value>(&source) {
                let _ = adf_to_text(&value);
            }
        }
    }

    fn fixture(source: &str) -> Value {
        serde_json::from_str(source).unwrap()
    }
}
