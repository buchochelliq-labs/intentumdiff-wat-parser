//! WebAssembly Text (WAT/WAST) parser plugin - full-parse mode.
//!
//! The parser owns a small S-expression reader so WAT/WAST support does not
//! depend on a Python tree-sitter package being installed. It emits the same
//! external semantic vocabulary used by the previous interpret-CST mapper:
//! module, func/start, import/export, type/memory/table/global/elem/data,
//! signature, instruction, identifier, and literal leaves.

use intentumdiff_plugin_sdk::tree::{SemanticNode, SemanticNodeBuilder};

wit_bindgen::generate!({
    path: "wit/plugin.wit",
    world: "parser-plugin",
});

use crate::exports::intentumdiff::plugin::parser::ExamplePair;
use crate::exports::intentumdiff::plugin::parser::Guest;
use crate::exports::intentumdiff::plugin::parser::LanguageInfoRecord;
use crate::exports::intentumdiff::plugin::parser::ParserMode;

const PLUGIN_METADATA: &str = include_str!("../plugin_metadata.info");

fn language_info_for(ids: Vec<String>) -> Vec<LanguageInfoRecord> {
    let metadata = intentumdiff_plugin_sdk::metadata::parse_plugin_metadata(PLUGIN_METADATA);
    ids.into_iter()
        .map(|language_id| {
            let info = metadata.language_or_default(&language_id);
            LanguageInfoRecord {
                language_id: info.language_id,
                language_name: info.language_name,
                language_short_name: info.language_short_name,
                monaco_language: info.monaco_language,
                default_filename: info.default_filename,
                language_file_extensions: info.language_file_extensions,
                author: metadata.author().to_string(),
                plugin_version: metadata.plugin_version().to_string(),
                last_updated: metadata.last_updated().to_string(),
            }
        })
        .collect()
}

struct WatParser;

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Open,
    Close,
    Atom,
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    text: String,
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
}

#[derive(Debug, Clone)]
enum Element {
    Atom(Token),
    Form(Form),
}

#[derive(Debug, Clone)]
struct Form {
    items: Vec<Element>,
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
}

fn bump_position(ch: char, line: &mut u32, col: &mut u32) {
    if ch == '\n' {
        *line += 1;
        *col = 0;
    } else {
        *col += 1;
    }
}

fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line = 0;
    let mut col = 0;

    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            bump_position(ch, &mut line, &mut col);
            i += 1;
            continue;
        }
        if ch == ';' && chars.get(i + 1) == Some(&';') {
            while i < chars.len() && chars[i] != '\n' {
                bump_position(chars[i], &mut line, &mut col);
                i += 1;
            }
            continue;
        }
        if ch == '(' && chars.get(i + 1) == Some(&';') {
            bump_position(chars[i], &mut line, &mut col);
            i += 1;
            bump_position(chars[i], &mut line, &mut col);
            i += 1;
            while i + 1 < chars.len() {
                if chars[i] == ';' && chars[i + 1] == ')' {
                    bump_position(chars[i], &mut line, &mut col);
                    i += 1;
                    bump_position(chars[i], &mut line, &mut col);
                    i += 1;
                    break;
                }
                bump_position(chars[i], &mut line, &mut col);
                i += 1;
            }
            continue;
        }
        if ch == '(' || ch == ')' {
            let start_line = line;
            let start_col = col;
            bump_position(ch, &mut line, &mut col);
            tokens.push(Token {
                kind: if ch == '(' {
                    TokenKind::Open
                } else {
                    TokenKind::Close
                },
                text: ch.to_string(),
                start_line,
                start_col,
                end_line: line,
                end_col: col,
            });
            i += 1;
            continue;
        }
        if ch == '"' {
            let start_line = line;
            let start_col = col;
            let mut text = String::new();
            // Consume the OPENING quote before scanning: the old loop broke on the very
            // first character (the opening quote itself), tokenizing every string as a
            // lone '"' and leaving its contents as a separate atom (#46 — export names
            // labeled '"' made string edits hash style-only).
            text.push(chars[i]);
            bump_position(chars[i], &mut line, &mut col);
            i += 1;
            let mut escaped = false;
            while i < chars.len() {
                let current = chars[i];
                text.push(current);
                bump_position(current, &mut line, &mut col);
                i += 1;
                if escaped {
                    escaped = false;
                } else if current == '\\' {
                    escaped = true;
                } else if current == '"' {
                    break;
                }
            }
            tokens.push(Token {
                kind: TokenKind::Atom,
                text,
                start_line,
                start_col,
                end_line: line,
                end_col: col,
            });
            continue;
        }

        let start_line = line;
        let start_col = col;
        let mut text = String::new();
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '(' && chars[i] != ')' {
            text.push(chars[i]);
            bump_position(chars[i], &mut line, &mut col);
            i += 1;
        }
        tokens.push(Token {
            kind: TokenKind::Atom,
            text,
            start_line,
            start_col,
            end_line: line,
            end_col: col,
        });
    }
    tokens
}

fn parse_form(tokens: &[Token], cursor: &mut usize) -> Option<Form> {
    let open = tokens.get(*cursor)?;
    if open.kind != TokenKind::Open {
        return None;
    }
    *cursor += 1;
    let mut items = Vec::new();
    let mut end_line = open.end_line;
    let mut end_col = open.end_col;

    while let Some(token) = tokens.get(*cursor) {
        match token.kind {
            TokenKind::Open => {
                if let Some(form) = parse_form(tokens, cursor) {
                    end_line = form.end_line;
                    end_col = form.end_col;
                    items.push(Element::Form(form));
                }
            }
            TokenKind::Close => {
                end_line = token.end_line;
                end_col = token.end_col;
                *cursor += 1;
                break;
            }
            TokenKind::Atom => {
                end_line = token.end_line;
                end_col = token.end_col;
                items.push(Element::Atom(token.clone()));
                *cursor += 1;
            }
        }
    }

    Some(Form {
        items,
        start_line: open.start_line,
        start_col: open.start_col,
        end_line,
        end_col,
    })
}

fn parse_forms(tokens: &[Token]) -> Vec<Form> {
    let mut cursor = 0;
    let mut forms = Vec::new();
    while cursor < tokens.len() {
        if tokens[cursor].kind == TokenKind::Open {
            if let Some(form) = parse_form(tokens, &mut cursor) {
                forms.push(form);
            }
        } else {
            cursor += 1;
        }
    }
    forms
}

fn head(form: &Form) -> Option<&str> {
    form.items.iter().find_map(|item| match item {
        Element::Atom(token) => Some(token.text.as_str()),
        Element::Form(_) => None,
    })
}

fn atoms_after_head(form: &Form) -> Vec<&Token> {
    let mut seen_head = false;
    let mut atoms = Vec::new();
    for item in &form.items {
        if let Element::Atom(token) = item {
            if !seen_head {
                seen_head = true;
            } else {
                atoms.push(token);
            }
        }
    }
    atoms
}

fn label_for(form: &Form, node_type: &str) -> String {
    let atoms = atoms_after_head(form);
    if node_type == "func" {
        if let Some(token) = atoms.iter().find(|token| token.text.starts_with('$')) {
            return token.text.clone();
        }
        return "(func)".to_string();
    }
    if node_type == "module" {
        if let Some(token) = atoms.iter().find(|token| token.text.starts_with('$')) {
            return token.text.clone();
        }
        return "module".to_string();
    }
    if matches!(node_type, "export" | "import") {
        if let Some(token) = atoms.iter().find(|token| token.text.starts_with('"')) {
            return token.text.clone();
        }
    }
    if let Some(token) = atoms.iter().find(|token| token.text.starts_with('$')) {
        return token.text.clone();
    }
    if let Some(token) = atoms.first() {
        token.text.clone()
    } else {
        node_type.to_string()
    }
}

fn map_head(value: &str) -> Option<&'static str> {
    match value {
        "module" => Some("module"),
        "func" => Some("func"),
        "start" => Some("start"),
        "import" => Some("import"),
        "export" => Some("export"),
        "type" => Some("type"),
        "memory" => Some("memory"),
        "table" => Some("table"),
        "global" => Some("global"),
        "elem" => Some("elem"),
        "data" => Some("data"),
        "param" | "result" | "local" => Some("signature"),
        _ => None,
    }
}

fn atom_node(id: &str, token: &Token, node_type: &str) -> SemanticNode {
    SemanticNodeBuilder::new(
        id,
        node_type,
        &token.text,
        token.start_line,
        token.start_col,
        token.end_line,
        token.end_col,
        "",
    )
    .build()
}

fn instruction_label(form: &Form) -> String {
    let mut parts = Vec::new();
    for item in &form.items {
        match item {
            Element::Atom(token) => parts.push(token.text.clone()),
            Element::Form(child) => parts.push(instruction_label(child)),
        }
    }
    parts.join(" ")
}

fn build_form(form: &Form, id: &str, parent_module: Option<&str>) -> SemanticNode {
    let mapped = head(form).and_then(map_head).unwrap_or("instruction");
    let label = if mapped == "instruction" {
        instruction_label(form)
    } else if mapped == "signature" {
        atoms_after_head(form)
            .iter()
            .map(|token| token.text.clone())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        label_for(form, mapped)
    };

    let module_label = if mapped == "module" {
        Some(label.as_str())
    } else {
        parent_module
    };

    let mut children = Vec::new();
    let mut child_index = 0usize;
    let mut seen_head = false;
    for item in &form.items {
        match item {
            Element::Atom(token) => {
                if !seen_head {
                    seen_head = true;
                    continue;
                }
                if mapped == "func" {
                    let node_type = if token.text.starts_with('$') {
                        "identifier"
                    } else if token.text.starts_with('"') {
                        "literal"
                    } else {
                        "instruction"
                    };
                    children.push(atom_node(
                        &format!("{}.{}", id, child_index),
                        token,
                        node_type,
                    ));
                    child_index += 1;
                }
            }
            Element::Form(child) => {
                let child_node =
                    build_form(child, &format!("{}.{}", id, child_index), module_label);
                children.push(child_node);
                child_index += 1;
            }
        }
    }

    let mut builder = SemanticNodeBuilder::new(
        id,
        mapped,
        label,
        form.start_line,
        form.start_col,
        form.end_line,
        form.end_col,
        "",
    )
    .children(children);
    if mapped != "module" {
        if let Some(module) = parent_module {
            builder = builder.parent_type(module.to_string());
        }
    }
    builder.build()
}

fn process_impl(source: &str) -> String {
    let tokens = tokenize(source);
    let forms = parse_forms(&tokens);
    let sem = if forms.len() == 1 && head(&forms[0]) == Some("module") {
        build_form(&forms[0], "0", None)
    } else {
        let children = forms
            .iter()
            .enumerate()
            .map(|(index, form)| build_form(form, &format!("0.{}", index), None))
            .collect();
        let end_line = source.lines().count() as u32;
        SemanticNodeBuilder::new("0", "source_file", "source_file", 1, 0, end_line, 0, "")
            .children(children)
            .build()
    };
    serde_json::to_string(&sem).unwrap_or_else(|e| format!(r#"{{"error":"Serialisation: {}"}}"#, e))
}

impl Guest for WatParser {
    fn get_parser_mode() -> ParserMode {
        ParserMode::FullParse
    }
    fn grammar_id() -> String {
        "wat".to_string()
    }
    fn detect_language(filename: String, _content: String) -> String {
        let lower = filename.to_lowercase();
        if lower.ends_with(".wat") {
            return "wat".to_string();
        }
        if lower.ends_with(".wast") {
            return "wast".to_string();
        }
        String::new()
    }
    fn preprocess_source(source: String) -> String {
        source
    }
    fn example(_language: String) -> ExamplePair {
        ExamplePair {
            old: "(module\n  (func $add (param i32 i32) (result i32)\n    local.get 0\n    local.get 1\n    i32.add)\n  (export \"add\" (func $add)))\n".to_string(),
            new: "(module\n  (func $add (param $a i32) (param $b i32) (result i32)\n    local.get $a\n    local.get $b\n    i32.add)\n  (func $multiply (param $a i32) (param $b i32) (result i32)\n    local.get $a\n    local.get $b\n    i32.mul)\n  (export \"add\" (func $add))\n  (export \"multiply\" (func $multiply)))\n".to_string(),
        }
    }
    fn process(input: String, _language: String, _filename: String) -> String {
        process_impl(&input)
    }
    fn trivia_node_types() -> Vec<String> {
        Vec::new()
    }
    fn language_ids() -> Vec<String> {
        vec!["wat".to_string(), "wast".to_string()]
    }
    fn language_info() -> Vec<LanguageInfoRecord> {
        language_info_for(Self::language_ids())
    }
    fn priority() -> i32 {
        0
    }
}

export!(WatParser);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exports::intentumdiff::plugin::parser::Guest;
    use intentumdiff_plugin_sdk::testing as t;

    #[test]
    fn grammar_id_nonempty() {
        assert!(!WatParser::grammar_id().is_empty());
    }

    #[test]
    fn language_ids_contain_grammar_id() {
        let gid = WatParser::grammar_id();
        let ids = WatParser::language_ids();
        assert!(
            ids.contains(&gid),
            "language_ids {:?} must contain {:?}",
            ids,
            gid
        );
    }

    #[test]
    fn parser_mode_is_full_parse() {
        assert!(matches!(
            WatParser::get_parser_mode(),
            ParserMode::FullParse
        ));
    }

    #[test]
    fn detect_language_known_ext() {
        let r = WatParser::detect_language("test.wat".to_string(), "".to_string());
        assert_eq!(r.as_str(), "wat");
        let r = WatParser::detect_language("test.wast".to_string(), "".to_string());
        assert_eq!(r.as_str(), "wast");
    }

    #[test]
    fn detect_language_unknown_ext() {
        let r =
            WatParser::detect_language("test.xyz_notareal_ext_9z8y".to_string(), "".to_string());
        assert_eq!(r.as_str(), "");
    }

    #[test]
    fn process_impl_empty_returns_valid_json() {
        let out = process_impl("");
        t::assert_valid_json(&out, "process(empty)");
    }

    #[test]
    fn process_impl_whitespace_returns_valid_json() {
        let out = process_impl("   \n  ");
        t::assert_valid_json(&out, "process(whitespace)");
    }

    #[test]
    fn playground_example_produces_module_functions_and_exports() {
        let example = <WatParser as Guest>::example("wat".to_string());
        let out = process_impl(&example.new);
        t::assert_valid_json(&out, "wat example");
        t::assert_no_error(&out, "wat example");
        t::assert_contains_node_type(&out, "module", "wat example");
        t::assert_contains_node_type(&out, "func", "wat example");
        t::assert_contains_node_type(&out, "export", "wat example");
        assert!(
            out.contains("$multiply"),
            "expected added function: {}",
            out
        );
    }
}
