use jira_ops::content::{ContentInput, compile_content};
use jira_ops::error::ErrorCode;
use serde_json::json;

#[test]
fn legacy_string_and_tagged_text_compile_identically() {
    let legacy: ContentInput = serde_json::from_value(json!("hello")).unwrap();
    let tagged: ContentInput =
        serde_json::from_value(json!({"format":"text","value":"hello"})).unwrap();
    assert_eq!(
        compile_content(&legacy).unwrap(),
        compile_content(&tagged).unwrap()
    );
}

#[test]
fn unsupported_markdown_fails_before_transport() {
    for value in [
        "<table><tr><td>x</td></tr></table>",
        "| column |\n| --- |\n| value |",
        "![alt](https://example.com/image.png)",
        "    indented code",
    ] {
        let input: ContentInput = serde_json::from_value(json!({
            "format":"markdown",
            "value":value
        }))
        .unwrap();
        assert_eq!(
            compile_content(&input).unwrap_err().code,
            ErrorCode::SchemaViolation,
            "{value:?}"
        );
    }
}

#[test]
fn markdown_depth_is_bounded() {
    let input: ContentInput = serde_json::from_value(json!({
        "format":"markdown",
        "value":format!("{}deep", "> ".repeat(65))
    }))
    .unwrap();
    assert_eq!(
        compile_content(&input).unwrap_err().code,
        ErrorCode::SchemaViolation
    );
}

#[test]
fn approved_markdown_compiles_to_bounded_adf() {
    let input: ContentInput = serde_json::from_value(json!({
        "format":"markdown",
        "value":"# Heading\n\n**strong** and [link](https://example.com)\n\n- one\n- two"
    }))
    .unwrap();
    assert_eq!(
        compile_content(&input).unwrap(),
        json!({
            "type":"doc",
            "version":1,
            "content":[
                {"type":"heading","attrs":{"level":1},"content":[{"type":"text","text":"Heading"}]},
                {"type":"paragraph","content":[
                    {"type":"text","text":"strong","marks":[{"type":"strong"}]},
                    {"type":"text","text":" and "},
                    {"type":"text","text":"link","marks":[{"type":"link","attrs":{"href":"https://example.com"}}]}
                ]},
                {"type":"bulletList","content":[
                    {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"one"}]}]},
                    {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"two"}]}]}
                ]}
            ]
        })
    );
}

#[test]
fn explicit_adf_is_validated_and_preserved() {
    let value = json!({"type":"doc","version":1,"content":[
        {"type":"paragraph","content":[{"type":"text","text":"hello"}]}
    ]});
    let input: ContentInput = serde_json::from_value(json!({
        "format":"adf",
        "value":value
    }))
    .unwrap();
    assert_eq!(compile_content(&input).unwrap(), value);
}
