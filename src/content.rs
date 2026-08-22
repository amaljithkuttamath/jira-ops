use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::adf::{adf_to_text, checked_text_to_adf};
use crate::commands::schema_violation;
use crate::error::AppError;

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_ADF_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(untagged)]
pub enum ContentInput {
    Legacy(String),
    Explicit { format: ContentFormat, value: Value },
}

impl ContentInput {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Legacy(value) => value.is_empty(),
            Self::Explicit { value, .. } => value.as_str().is_some_and(str::is_empty),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Legacy(value) => value.len(),
            Self::Explicit { value, .. } => value.as_str().map_or(0, str::len),
        }
    }

    pub fn source_text(&self) -> Option<&str> {
        match self {
            Self::Legacy(value) => Some(value),
            Self::Explicit { value, .. } => value.as_str(),
        }
    }

    pub fn format(&self) -> ContentFormat {
        match self {
            Self::Legacy(_) => ContentFormat::Text,
            Self::Explicit { format, .. } => *format,
        }
    }
}

impl From<String> for ContentInput {
    fn from(value: String) -> Self {
        Self::Legacy(value)
    }
}

impl From<&str> for ContentInput {
    fn from(value: &str) -> Self {
        Self::Legacy(value.to_owned())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentFormat {
    Text,
    Markdown,
    Adf,
}

pub fn compile_content(input: &ContentInput) -> Result<Value, AppError> {
    let compiled = match input {
        ContentInput::Legacy(text) => compile_text(text)?,
        ContentInput::Explicit {
            format: ContentFormat::Text,
            value,
        } => compile_text(require_string(value)?)?,
        ContentInput::Explicit {
            format: ContentFormat::Markdown,
            value,
        } => markdown_to_adf(require_string(value)?)?,
        ContentInput::Explicit {
            format: ContentFormat::Adf,
            value,
        } => validate_adf(value)?,
    };
    ensure_encoded_limit(&compiled)?;
    Ok(compiled)
}

fn compile_text(text: &str) -> Result<Value, AppError> {
    validate_source(text)?;
    checked_text_to_adf(text)
}

fn require_string(value: &Value) -> Result<&str, AppError> {
    value
        .as_str()
        .ok_or_else(|| schema_violation("text and markdown content values must be strings"))
}

fn validate_source(source: &str) -> Result<(), AppError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(schema_violation("content source exceeds the 1 MiB limit"));
    }
    if source
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(schema_violation(
            "content source contains unsupported controls",
        ));
    }
    Ok(())
}

fn validate_adf(value: &Value) -> Result<Value, AppError> {
    if value.get("type").and_then(Value::as_str) != Some("doc")
        || value.get("version").and_then(Value::as_u64) != Some(1)
        || !value.get("content").is_some_and(Value::is_array)
    {
        return Err(schema_violation("ADF content must be a version 1 document"));
    }
    let mut stack = vec![(value, 0_usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(schema_violation("ADF content exceeds the depth limit"));
        }
        match node {
            Value::String(text) => validate_source(text)?,
            Value::Array(values) => {
                stack.extend(values.iter().map(|child| (child, depth + 1)));
            }
            Value::Object(values) => {
                stack.extend(values.values().map(|child| (child, depth + 1)));
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    adf_to_text(value).map_err(|_| schema_violation("ADF content is invalid"))?;
    ensure_encoded_limit(value)?;
    Ok(value.clone())
}

#[derive(Debug)]
enum FrameKind {
    Doc,
    Paragraph,
    Heading(u8),
    Blockquote,
    BulletList,
    OrderedList(u64),
    ListItem,
    CodeBlock(Option<String>),
}

#[derive(Debug)]
struct Frame {
    kind: FrameKind,
    content: Vec<Value>,
    code: String,
}

impl Frame {
    fn new(kind: FrameKind) -> Self {
        Self {
            kind,
            content: Vec::new(),
            code: String::new(),
        }
    }

    fn into_node(self) -> Value {
        match self.kind {
            FrameKind::Doc => json!({"type":"doc","version":1,"content":self.content}),
            FrameKind::Paragraph => json!({"type":"paragraph","content":self.content}),
            FrameKind::Heading(level) => {
                json!({"type":"heading","attrs":{"level":level},"content":self.content})
            }
            FrameKind::Blockquote => json!({"type":"blockquote","content":self.content}),
            FrameKind::BulletList => json!({"type":"bulletList","content":self.content}),
            FrameKind::OrderedList(order) => {
                json!({"type":"orderedList","attrs":{"order":order},"content":self.content})
            }
            FrameKind::ListItem => {
                json!({"type":"listItem","content":normalize_list_item(self.content)})
            }
            FrameKind::CodeBlock(language) => {
                let content = if self.code.is_empty() {
                    Vec::new()
                } else {
                    vec![json!({"type":"text","text":self.code})]
                };
                match language {
                    Some(language) => json!({
                        "type":"codeBlock",
                        "attrs":{"language":language},
                        "content":content
                    }),
                    None => json!({"type":"codeBlock","content":content}),
                }
            }
        }
    }
}

fn normalize_list_item(content: Vec<Value>) -> Vec<Value> {
    let mut blocks = Vec::new();
    let mut inline = Vec::new();
    for node in content {
        let kind = node.get("type").and_then(Value::as_str);
        if matches!(kind, Some("text" | "hardBreak")) {
            inline.push(node);
        } else {
            if !inline.is_empty() {
                blocks.push(json!({"type":"paragraph","content":std::mem::take(&mut inline)}));
            }
            blocks.push(node);
        }
    }
    if !inline.is_empty() {
        blocks.push(json!({"type":"paragraph","content":inline}));
    }
    blocks
}

fn markdown_to_adf(source: &str) -> Result<Value, AppError> {
    validate_source(source)?;
    let mut frames = vec![Frame::new(FrameKind::Doc)];
    let mut marks = Vec::<Value>::new();
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT;
    for event in Parser::new_ext(source, options) {
        match event {
            Event::Start(tag) => start_tag(tag, &mut frames, &mut marks)?,
            Event::End(tag) => end_tag(tag, &mut frames, &mut marks)?,
            Event::Text(text) => append_text(&text, &mut frames, &marks)?,
            Event::Code(text) => append_inline_code(&text, &mut frames, &marks)?,
            Event::SoftBreak | Event::HardBreak => append_break(&mut frames)?,
            Event::Html(_)
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_)
            | Event::Rule
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => return Err(unsupported_markdown()),
        }
    }
    if frames.len() != 1 || !marks.is_empty() {
        return Err(schema_violation("markdown content is structurally invalid"));
    }
    let adf = frames.pop().expect("document frame exists").into_node();
    ensure_encoded_limit(&adf)?;
    Ok(adf)
}

fn start_tag(
    tag: Tag<'_>,
    frames: &mut Vec<Frame>,
    marks: &mut Vec<Value>,
) -> Result<(), AppError> {
    match tag {
        Tag::Paragraph => push_frame(frames, FrameKind::Paragraph)?,
        Tag::Heading { level, .. } => push_frame(frames, FrameKind::Heading(heading_level(level)))?,
        Tag::BlockQuote(None) => push_frame(frames, FrameKind::Blockquote)?,
        Tag::CodeBlock(CodeBlockKind::Fenced(info)) => {
            let language = info
                .split_whitespace()
                .next()
                .filter(|value| !value.is_empty());
            push_frame(
                frames,
                FrameKind::CodeBlock(language.map(ToOwned::to_owned)),
            )?
        }
        Tag::List(start) => push_frame(
            frames,
            start.map_or(FrameKind::BulletList, FrameKind::OrderedList),
        )?,
        Tag::Item => push_frame(frames, FrameKind::ListItem)?,
        Tag::Emphasis => marks.push(json!({"type":"em"})),
        Tag::Strong => marks.push(json!({"type":"strong"})),
        Tag::Link { dest_url, .. } => {
            let href = validate_link(&dest_url)?;
            marks.push(json!({"type":"link","attrs":{"href":href}}));
        }
        Tag::BlockQuote(Some(_))
        | Tag::CodeBlock(CodeBlockKind::Indented)
        | Tag::HtmlBlock
        | Tag::FootnoteDefinition(_)
        | Tag::DefinitionList
        | Tag::DefinitionListTitle
        | Tag::DefinitionListDefinition
        | Tag::Table(_)
        | Tag::TableHead
        | Tag::TableRow
        | Tag::TableCell
        | Tag::Strikethrough
        | Tag::Superscript
        | Tag::Subscript
        | Tag::Image { .. }
        | Tag::MetadataBlock(_) => return Err(unsupported_markdown()),
    }
    Ok(())
}

fn end_tag(tag: TagEnd, frames: &mut Vec<Frame>, marks: &mut Vec<Value>) -> Result<(), AppError> {
    match tag {
        TagEnd::Paragraph
        | TagEnd::Heading(_)
        | TagEnd::BlockQuote(None)
        | TagEnd::CodeBlock
        | TagEnd::List(_)
        | TagEnd::Item => close_frame(frames),
        TagEnd::Emphasis | TagEnd::Strong | TagEnd::Link => {
            marks
                .pop()
                .ok_or_else(|| schema_violation("markdown marks are unbalanced"))?;
        }
        TagEnd::BlockQuote(Some(_))
        | TagEnd::HtmlBlock
        | TagEnd::FootnoteDefinition
        | TagEnd::DefinitionList
        | TagEnd::DefinitionListTitle
        | TagEnd::DefinitionListDefinition
        | TagEnd::Table
        | TagEnd::TableHead
        | TagEnd::TableRow
        | TagEnd::TableCell
        | TagEnd::Strikethrough
        | TagEnd::Superscript
        | TagEnd::Subscript
        | TagEnd::Image
        | TagEnd::MetadataBlock(_) => return Err(unsupported_markdown()),
    }
    Ok(())
}

fn push_frame(frames: &mut Vec<Frame>, kind: FrameKind) -> Result<(), AppError> {
    if frames.len() > MAX_DEPTH {
        return Err(schema_violation("markdown content exceeds the depth limit"));
    }
    frames.push(Frame::new(kind));
    Ok(())
}

fn close_frame(frames: &mut Vec<Frame>) {
    let node = frames
        .pop()
        .expect("markdown tags are balanced")
        .into_node();
    frames
        .last_mut()
        .expect("document frame remains")
        .content
        .push(node);
}

fn append_text(text: &str, frames: &mut [Frame], marks: &[Value]) -> Result<(), AppError> {
    validate_source(text)?;
    let frame = frames.last_mut().expect("document frame exists");
    if matches!(frame.kind, FrameKind::CodeBlock(_)) {
        frame.code.push_str(text);
        return Ok(());
    }
    let mut node = json!({"type":"text","text":text});
    if !marks.is_empty() {
        node.as_object_mut()
            .expect("text node is an object")
            .insert("marks".to_owned(), Value::Array(marks.to_vec()));
    }
    frame.content.push(node);
    Ok(())
}

fn append_inline_code(text: &str, frames: &mut [Frame], marks: &[Value]) -> Result<(), AppError> {
    let mut code_marks = marks.to_vec();
    code_marks.push(json!({"type":"code"}));
    append_text(text, frames, &code_marks)
}

fn append_break(frames: &mut [Frame]) -> Result<(), AppError> {
    let frame = frames.last_mut().expect("document frame exists");
    if matches!(frame.kind, FrameKind::CodeBlock(_)) {
        frame.code.push('\n');
    } else {
        frame.content.push(json!({"type":"hardBreak"}));
    }
    Ok(())
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn validate_link(value: &str) -> Result<String, AppError> {
    let url = url::Url::parse(value).map_err(|_| schema_violation("markdown link is invalid"))?;
    if !matches!(url.scheme(), "http" | "https" | "mailto") {
        return Err(schema_violation("markdown link scheme is unsupported"));
    }
    Ok(value.to_owned())
}

fn ensure_encoded_limit(value: &Value) -> Result<(), AppError> {
    if serde_json::to_vec(value)
        .map_err(|_| schema_violation("content could not be encoded"))?
        .len()
        > MAX_ADF_BYTES
    {
        return Err(schema_violation("encoded ADF exceeds the 4 MiB limit"));
    }
    Ok(())
}

fn unsupported_markdown() -> AppError {
    schema_violation("markdown contains a node that Jira Ops does not support")
}
