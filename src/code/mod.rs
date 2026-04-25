//! Code intelligence (YYC-44 epic).
//!
//! Tree-sitter-backed parsing today (YYC-45); LSP layer (YYC-46) lands
//! alongside. Both share `Language` here so language detection has one
//! source of truth.

use std::path::Path;
use std::sync::Mutex;
use tree_sitter::{Language as TsLanguage, Parser as TsParser};

/// Languages with first-class structural support. Extend by adding a
/// variant + extension match + grammar in `parser_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Json,
}

impl Language {
    /// Detect by file extension. Path is canonicalized through
    /// `extension()` so case differences ("FOO.RS") still hit.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())?;
        match ext.as_str() {
            "rs" => Some(Self::Rust),
            "py" | "pyi" => Some(Self::Python),
            "ts" | "tsx" => Some(Self::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            "go" => Some(Self::Go),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Go => "go",
            Self::Json => "json",
        }
    }

    /// Tree-sitter grammar for this language.
    fn grammar(self) -> TsLanguage {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
        }
    }

    /// Tree-sitter S-expression query that matches top-level
    /// definitions interesting to a code outline. Captures `name` (the
    /// symbol name) and `kind` (a literal so the consumer can tell
    /// `function` from `struct` etc.).
    pub fn outline_query(self) -> &'static str {
        match self {
            Self::Rust => OUTLINE_RUST,
            Self::Python => OUTLINE_PYTHON,
            Self::TypeScript => OUTLINE_TS,
            Self::JavaScript => OUTLINE_JS,
            Self::Go => OUTLINE_GO,
            Self::Json => "",
        }
    }
}

const OUTLINE_RUST: &str = r#"
(function_item name: (identifier) @name) @function
(struct_item name: (type_identifier) @name) @struct
(enum_item name: (type_identifier) @name) @enum
(trait_item name: (type_identifier) @name) @trait
(impl_item type: (type_identifier) @name) @impl
(mod_item name: (identifier) @name) @module
(const_item name: (identifier) @name) @const
(static_item name: (identifier) @name) @static
(type_item name: (type_identifier) @name) @typedef
"#;

const OUTLINE_PYTHON: &str = r#"
(function_definition name: (identifier) @name) @function
(class_definition name: (identifier) @name) @class
"#;

const OUTLINE_TS: &str = r#"
(function_declaration name: (identifier) @name) @function
(class_declaration name: (type_identifier) @name) @class
(interface_declaration name: (type_identifier) @name) @interface
(type_alias_declaration name: (type_identifier) @name) @typedef
(enum_declaration name: (identifier) @name) @enum
"#;

const OUTLINE_JS: &str = r#"
(function_declaration name: (identifier) @name) @function
(class_declaration name: (identifier) @name) @class
"#;

const OUTLINE_GO: &str = r#"
(function_declaration name: (identifier) @name) @function
(method_declaration name: (field_identifier) @name) @method
(type_declaration (type_spec name: (type_identifier) @name)) @type
"#;

/// Lazy-initialized per-language parser cache. Cheap to clone the
/// `Arc<ParserCache>` but parsers themselves aren't `Send + Sync` once
/// borrowed mutably — wrap each in a Mutex.
pub struct ParserCache {
    rust: Mutex<Option<TsParser>>,
    python: Mutex<Option<TsParser>>,
    typescript: Mutex<Option<TsParser>>,
    javascript: Mutex<Option<TsParser>>,
    go: Mutex<Option<TsParser>>,
    json: Mutex<Option<TsParser>>,
}

impl ParserCache {
    pub fn new() -> Self {
        Self {
            rust: Mutex::new(None),
            python: Mutex::new(None),
            typescript: Mutex::new(None),
            javascript: Mutex::new(None),
            go: Mutex::new(None),
            json: Mutex::new(None),
        }
    }

    /// Run `f` against a parser configured for `lang`. The parser is
    /// initialized on first use and reused across calls.
    pub fn with_parser<R>(
        &self,
        lang: Language,
        f: impl FnOnce(&mut TsParser) -> R,
    ) -> anyhow::Result<R> {
        let slot = match lang {
            Language::Rust => &self.rust,
            Language::Python => &self.python,
            Language::TypeScript => &self.typescript,
            Language::JavaScript => &self.javascript,
            Language::Go => &self.go,
            Language::Json => &self.json,
        };
        let mut guard = slot.lock().unwrap();
        if guard.is_none() {
            let mut p = TsParser::new();
            p.set_language(&lang.grammar())
                .map_err(|e| anyhow::anyhow!("set_language for {}: {e}", lang.name()))?;
            *guard = Some(p);
        }
        Ok(f(guard.as_mut().unwrap()))
    }
}

impl Default for ParserCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn language_detection_by_extension() {
        assert_eq!(
            Language::from_path(&PathBuf::from("foo.rs")),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::from_path(&PathBuf::from("FOO.RS")),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::from_path(&PathBuf::from("a/b.py")),
            Some(Language::Python)
        );
        assert_eq!(
            Language::from_path(&PathBuf::from("a/b.tsx")),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::from_path(&PathBuf::from("a/b.unknown")),
            None
        );
    }

    #[test]
    fn parser_cache_initializes_lazily_and_reuses() {
        let cache = ParserCache::new();
        let parsed_first = cache
            .with_parser(Language::Rust, |p| p.parse("fn main() {}", None).is_some())
            .unwrap();
        assert!(parsed_first);
        let parsed_again = cache
            .with_parser(Language::Rust, |p| p.parse("fn other() {}", None).is_some())
            .unwrap();
        assert!(parsed_again);
    }
}
