//! Definition and import line classification.
//!
//! Every matched line is read with a small per-language grammar: skip the
//! indentation, attributes and modifiers, look the leading keyword up and
//! read the defined name. A match is a definition only when it touches the
//! declaration header (first modifier through the name), so for
//! `fn render(frame: Frame)` a search for `render` is a definition and a
//! search for `Frame` is a usage.
//!
//! The scanning is byte level: blanks and identifier runs are classified 16
//! bytes at a time (SSE2/NEON, see [`scan`]) and tokens are compared as packed
//! `u128` words, so a line costs a handful of vector compares and integer
//! comparisons, no allocation and no UTF-8 decoding.

mod grammar;
mod scan;

pub use grammar::Lang;

use super::types::GrepMatch;
use crate::simd_path::ArenaPtr;
use crate::types::FileItem;
use grammar::{
    ASYNC, Attributes, DEFINE, ENUM, EXPORT, EXTERN, FINAL, FUN, FUNCTION, Grammar, LOCAL, MUT,
    OPAQUE, PACKED, PUB, Rule, STRUCT, TEMPLATE, UNION, is_control,
};
use scan::{ident_end, is_blank, is_ident, skip_blank, word};

/// What a definition line defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefinitionKind {
    Function,
    /// struct, class, enum, trait, interface, type alias...
    Type,
    /// Rust `impl`, Swift `extension`, Haskell `instance`.
    Impl,
    Module,
    Constant,
    Variable,
    Macro,
}

/// A definition found on a line. Offsets are byte offsets into the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Definition {
    pub kind: DefinitionKind,
    /// Start of the declaration: the first modifier or the keyword.
    pub start: u32,
    /// Byte range of the defined name.
    pub name: (u32, u32),
}

impl Definition {
    /// Whether any `(start, end)` byte range touches the declaration header,
    /// from the first modifier through the name. Matches in the parameters,
    /// the return type or the body are usages.
    #[inline]
    pub fn is_hit_by(&self, ranges: &[(u32, u32)]) -> bool {
        ranges
            .iter()
            .any(|&(start, end)| start < self.name.1 && end > self.start)
    }
}

/// Classify a single line of `lang` source.
///
/// ```
/// use fff_search::grep::{DefinitionKind, Lang, classify_line};
///
/// let def = classify_line("    pub(crate) async fn render(frame: Frame) {", Lang::Rust).unwrap();
/// assert_eq!(def.kind, DefinitionKind::Function);
/// assert_eq!(def.name, (24, 30));
/// assert!(classify_line("    let x = render(frame);", Lang::Rust).is_none());
/// ```
pub fn classify_line(line: &str, lang: Lang) -> Option<Definition> {
    if lang == Lang::Text {
        return None;
    }
    Parser {
        s: line.as_bytes(),
        lang,
        g: lang.grammar(),
    }
    .run()
}

/// Detect if a line looks like a code definition in any language.
pub fn is_definition_line(line: &str) -> bool {
    classify_line(line, Lang::Unknown).is_some()
}

/// Set `is_definition` on every match of one file: the line defines something
/// and one of the match ranges touches its declaration header.
pub(crate) fn mark_definitions(lang: Lang, matches: &mut [GrepMatch]) {
    for m in matches {
        m.is_definition = classify_line(&m.line_content, lang)
            .is_some_and(|def| def.is_hit_by(&m.match_byte_offsets));
    }
}

/// Language of a grep candidate, from its file name.
pub(crate) fn file_lang(file: &FileItem, arena: ArenaPtr) -> Lang {
    let mut name = String::with_capacity(64);
    file.write_file_name_from_arena(arena, &mut name);
    Lang::from_file_name(&name)
}

type Span = (usize, usize);

#[inline]
fn definition(kind: DefinitionKind, start: usize, name: Span) -> Definition {
    Definition {
        kind,
        start: start as u32,
        name: (name.0 as u32, name.1 as u32),
    }
}

/// Index just past the `close` matching the `open` at `s[i]`; `None` when the
/// line ends first. `->` and `=>` do not close a `<`.
fn skip_balanced(s: &[u8], i: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0usize;
    for (j, &b) in s.iter().enumerate().skip(i) {
        if b == open {
            depth += 1;
        } else if b == close && !(close == b'>' && j > 0 && matches!(s[j - 1], b'-' | b'=')) {
            depth -= 1;
            if depth == 0 {
                return Some(j + 1);
            }
        }
    }
    None
}

struct Parser<'a> {
    s: &'a [u8],
    lang: Lang,
    g: &'static Grammar,
}

impl Parser<'_> {
    fn run(&self) -> Option<Definition> {
        let s = self.s;
        let indent = skip_blank(s, 0);
        if indent == s.len() {
            return None;
        }
        if let Some(def) = self.prelude(indent) {
            return Some(def);
        }

        let start = self.skip_attributes(indent)?;
        let mut i = start;
        let mut modifiers = 0;
        let mut exported = false;
        loop {
            let end = ident_end(s, i);
            let w = word(s, i, end);
            if w == 0 {
                break;
            }
            let next = skip_blank(s, end);
            if w == PUB && self.lang == Lang::Rust && s.get(end) == Some(&b'(') {
                // pub(crate), pub(in path)
                i = skip_blank(s, skip_balanced(s, end, b'(', b')')?);
            } else if w == TEMPLATE && self.lang == Lang::C && s.get(next) == Some(&b'<') {
                i = skip_blank(s, skip_balanced(s, next, b'<', b'>')?);
            } else if next > end && self.g.is_modifier(w) {
                i = next;
                if w == EXTERN && s.get(i) == Some(&b'"') {
                    // extern "C"
                    let close = memchr::memchr(b'"', &s[i + 1..])?;
                    i = skip_blank(s, i + close + 2);
                }
            } else if next > end
                && self.g.rule(w).is_some_and(|r| r != Rule::Typedef)
                && self.keyword_follows(next)
            {
                // a definition keyword used as a modifier: `const fn`,
                // `enum class`, `module type`, `class func`
                i = next;
            } else {
                break;
            }
            exported |= w == PUB || w == EXPORT;
            modifiers += 1;
        }

        let end = ident_end(s, i);
        if let Some(rule) = self.g.rule(word(s, i, end))
            && let Some(def) = self.apply(rule, start, (i, end), indent, exported)
        {
            return Some(def);
        }
        if self.g.c_like_functions
            && let Some(name) = self.c_like_function(i, modifiers)
        {
            return Some(definition(DefinitionKind::Function, start, name));
        }
        if self.g.methods
            && indent > 0
            && let Some(name) = self.method(i)
        {
            return Some(definition(DefinitionKind::Function, start, name));
        }
        None
    }

    /// Whether a definition keyword comes at `i`, possibly after modifiers.
    fn keyword_follows(&self, i: usize) -> bool {
        let i = self.skip_modifier_words(i);
        self.g.rule(word(self.s, i, ident_end(self.s, i))).is_some()
    }

    /// Skip modifier words (`rec` in `let rec f`, `mut` in `static mut X`).
    fn skip_modifier_words(&self, mut i: usize) -> usize {
        loop {
            let end = ident_end(self.s, i);
            let w = word(self.s, i, end);
            let next = skip_blank(self.s, end);
            let modifier = self.g.is_modifier(w) || (w == MUT && self.lang == Lang::Rust);
            if w == 0 || next == end || !modifier {
                return i;
            }
            i = next;
        }
    }

    fn apply(
        &self,
        rule: Rule,
        start: usize,
        keyword: Span,
        indent: usize,
        exported: bool,
    ) -> Option<Definition> {
        let s = self.s;
        let after = keyword.1;
        let def = match rule {
            Rule::Named(kind) => {
                let i = self.skip_modifier_words(self.name_start(after)?);
                definition(kind, start, self.name(i)?)
            }
            Rule::Generic(kind) => {
                let mut i = skip_blank(s, after);
                if s.get(i) == Some(&b'<') {
                    i = skip_blank(s, skip_balanced(s, i, b'<', b'>')?);
                } else if i == after {
                    return None;
                }
                let mut name = self.name(i)?;
                // generic receiver: `fun <T> List<T>.chunked`
                if s.get(name.1) == Some(&b'<')
                    && let Some(close) = skip_balanced(s, name.1, b'<', b'>')
                    && s.get(close) == Some(&b'.')
                    && let Some(method) = self.name(close + 1)
                {
                    name.1 = method.1;
                }
                definition(kind, start, name)
            }
            Rule::TopLevel(kind) => {
                if indent > 0 && !exported {
                    return None;
                }
                let name = self.name(self.skip_modifier_words(self.name_start(after)?))?;
                definition(self.binding_kind(kind, name.1), start, name)
            }
            Rule::CType => {
                let name = self.name(self.name_start(after)?)?;
                let k = skip_blank(s, name.1);
                let defines = match s.get(k) {
                    None | Some(b'{' | b':' | b'<') => true,
                    Some(_) => word(s, k, ident_end(s, k)) == FINAL,
                };
                if !defines {
                    return None;
                }
                definition(DefinitionKind::Type, start, name)
            }
            Rule::GoFunc => {
                let mut i = skip_blank(s, after);
                if s.get(i) == Some(&b'(') {
                    // method receiver
                    i = skip_blank(s, skip_balanced(s, i, b'(', b')')?);
                }
                definition(DefinitionKind::Function, start, self.name(i)?)
            }
            Rule::Impl => {
                let mut i = skip_blank(s, after);
                if s.get(i) == Some(&b'<') {
                    i = skip_blank(s, skip_balanced(s, i, b'<', b'>')?);
                } else if i == after {
                    return None;
                }
                let mut end = memchr::memchr(b'{', &s[i..]).map_or(s.len(), |p| i + p);
                if let Some(p) = memchr::memmem::find(&s[i..end], b" where") {
                    end = i + p;
                }
                while end > i && is_blank(s[end - 1]) {
                    end -= 1;
                }
                if end == i {
                    return None;
                }
                definition(DefinitionKind::Impl, start, (i, end))
            }
            Rule::MacroBang => {
                if s.get(after) != Some(&b'!') {
                    return None;
                }
                let name = self.name(skip_blank(s, after + 1))?;
                definition(DefinitionKind::Macro, start, name)
            }
            Rule::Typedef => definition(DefinitionKind::Type, start, self.typedef_name(after)?),
            Rule::Bare(kind) => definition(kind, start, keyword),
        };
        Some(def)
    }

    /// Start of the name after a keyword: at least one blank, or a `*`
    /// (JavaScript generators: `function* gen`, `function *gen`).
    fn name_start(&self, after: usize) -> Option<usize> {
        let s = self.s;
        let i = skip_blank(s, after);
        if s.get(i) == Some(&b'*') {
            return Some(skip_blank(s, i + 1));
        }
        (i > after).then_some(i)
    }

    /// An identifier at `i`, extended over `Foo.bar` / `Foo::bar` paths and
    /// `name?` suffixes where the language has them.
    fn name(&self, mut i: usize) -> Option<Span> {
        let s = self.s;
        let start = i;
        if self.lang == Lang::Rust && s[i..].starts_with(b"r#") {
            i += 2;
        }
        if !s
            .get(i)
            .is_some_and(|&b| is_ident(b) && !b.is_ascii_digit())
        {
            return None;
        }
        let mut end = ident_end(s, i);
        if self.g.path_names {
            loop {
                let sep = match s.get(end) {
                    Some(b'.') => 1,
                    Some(b':') if s.get(end + 1) == Some(&b':') => 2,
                    Some(b':') if self.g.colon_paths => 1,
                    _ => break,
                };
                if !s.get(end + sep).is_some_and(|&b| is_ident(b)) {
                    break;
                }
                end = ident_end(s, end + sep);
            }
        }
        if self.g.predicate_suffix && matches!(s.get(end), Some(b'?' | b'!')) {
            end += 1;
        }
        Some((start, end))
    }

    /// Refine `const x = ...`: Zig `const Foo = struct {` is a type, JS
    /// `const f = () => ...` and `const f = function` are functions.
    fn binding_kind(&self, kind: DefinitionKind, name_end: usize) -> DefinitionKind {
        let s = self.s;
        let Some(eq) = memchr::memchr(b'=', &s[name_end..]).map(|p| name_end + p) else {
            return kind;
        };
        let r = skip_blank(s, eq + 1);
        match word(s, r, ident_end(s, r)) {
            STRUCT | ENUM | UNION | OPAQUE | PACKED | EXTERN if self.lang == Lang::Zig => {
                DefinitionKind::Type
            }
            FUNCTION | FUN | ASYNC => DefinitionKind::Function,
            _ if s.get(r) == Some(&b'(') && memchr::memmem::find(&s[r..], b"=>").is_some() => {
                DefinitionKind::Function
            }
            _ => kind,
        }
    }

    /// `typedef struct foo foo_t;` names `foo_t`, `typedef void (*cb)(int);` names `cb`.
    fn typedef_name(&self, after: usize) -> Option<Span> {
        let s = self.s;
        if let Some(p) = memchr::memmem::find(&s[after..], b"(*") {
            let i = skip_blank(s, after + p + 2);
            return self.name(i);
        }
        let mut end = s.len();
        while end > after && (is_blank(s[end - 1]) || s[end - 1] == b';') {
            end -= 1;
        }
        // array typedefs: `typedef int vec3[3];`
        while end > after && s[end - 1] == b']' {
            end = after + memchr::memrchr(b'[', &s[after..end])?;
        }
        let mut start = end;
        while start > after && is_ident(s[start - 1]) {
            start -= 1;
        }
        (start < end && start > after).then_some((start, end))
    }

    /// Forms that do not start with a keyword.
    fn prelude(&self, indent: usize) -> Option<Definition> {
        let s = self.s;
        match self.lang {
            // #define NAME
            Lang::C if s[indent] == b'#' => {
                let i = skip_blank(s, indent + 1);
                let end = ident_end(s, i);
                if word(s, i, end) != DEFINE {
                    return None;
                }
                let name = self.name(self.name_start(end)?)?;
                Some(definition(DefinitionKind::Macro, indent, name))
            }
            // name() { ... }, names may contain `-`, `:` and `.`
            Lang::Shell => {
                let end = s[indent..]
                    .iter()
                    .position(|&b| !(is_ident(b) || matches!(b, b'-' | b':' | b'.')))
                    .map_or(s.len(), |p| indent + p);
                let open = skip_blank(s, end);
                if end == indent || s.get(open) != Some(&b'(') {
                    return None;
                }
                let close = skip_blank(s, open + 1);
                (s.get(close) == Some(&b')'))
                    .then(|| definition(DefinitionKind::Function, indent, (indent, end)))
            }
            // name :: Type
            Lang::Haskell if indent == 0 => {
                let name = self.name(0)?;
                let k = skip_blank(s, name.1);
                (s[k..].starts_with(b"::") && !s[k..].starts_with(b":::"))
                    .then(|| definition(DefinitionKind::Function, 0, name))
            }
            // [local] M.name = function(...)
            Lang::Lua => {
                let mut i = indent;
                let end = ident_end(s, i);
                if word(s, i, end) == LOCAL && s.get(end).is_some_and(|&b| is_blank(b)) {
                    i = skip_blank(s, end);
                }
                let name = self.name(i)?;
                let eq = skip_blank(s, name.1);
                if s.get(eq) != Some(&b'=') || s.get(eq + 1) == Some(&b'=') {
                    return None;
                }
                let r = skip_blank(s, eq + 1);
                let r_end = ident_end(s, r);
                (word(s, r, r_end) == FUNCTION)
                    .then(|| definition(DefinitionKind::Function, indent, name))
            }
            _ => None,
        }
    }

    fn skip_attributes(&self, mut i: usize) -> Option<usize> {
        let s = self.s;
        loop {
            let first = s.get(i).copied();
            let second = s.get(i + 1).copied();
            let end = match (self.g.attributes, first, second) {
                (Attributes::Hash, Some(b'#'), Some(b'[')) => skip_balanced(s, i + 1, b'[', b']')?,
                (Attributes::Hash, Some(b'#'), Some(b'!')) if s.get(i + 2) == Some(&b'[') => {
                    skip_balanced(s, i + 2, b'[', b']')?
                }
                (Attributes::At | Attributes::AtAndBrackets, Some(b'@'), Some(b))
                    if is_ident(b) =>
                {
                    let mut j = ident_end(s, i + 1);
                    while s.get(j) == Some(&b'.') && s.get(j + 1).is_some_and(|&b| is_ident(b)) {
                        j = ident_end(s, j + 1);
                    }
                    if s.get(j) == Some(&b'(') {
                        j = skip_balanced(s, j, b'(', b')')?;
                    }
                    j
                }
                (Attributes::AtAndBrackets, Some(b'['), _)
                | (Attributes::DoubleBrackets, Some(b'['), Some(b'[')) => {
                    skip_balanced(s, i, b'[', b']')?
                }
                _ => return Some(i),
            };
            i = skip_blank(s, end);
        }
    }

    /// `Type name(...)` without a keyword (C, C++, Java, C#, Dart). Needs a
    /// type before the name unless a modifier was seen (constructors), and
    /// rejects what follows a call or a prototype: `;`, `,`, `.`, `)`, `=`.
    fn c_like_function(&self, mut i: usize, modifiers: usize) -> Option<Span> {
        let s = self.s;
        let mut idents = 0;
        let mut last = None;
        loop {
            i = skip_blank(s, i);
            let b = *s.get(i)?;
            if is_ident(b) {
                let end = ident_end(s, i);
                if idents == 0 && is_control(word(s, i, end)) {
                    return None;
                }
                idents += 1;
                last = Some((i, end));
                i = end;
                continue;
            }
            match b {
                b'(' => break,
                b'*' | b'&' | b'~' | b'?' | b'^' => i += 1,
                b':' if s.get(i + 1) == Some(&b':') => i += 2,
                b'<' => i = skip_balanced(s, i, b'<', b'>')?,
                b'[' => i = skip_balanced(s, i, b'[', b']')?,
                _ => return None,
            }
        }
        let name = last?;
        if skip_blank(s, name.1) != i || (idents < 2 && modifiers == 0) {
            return None;
        }
        if let Some(close) = skip_balanced(s, i, b'(', b')') {
            let k = skip_blank(s, close);
            match s.get(k) {
                Some(b';' | b',' | b')' | b'.') => return None,
                Some(b'=') if s.get(k + 1) != Some(&b'>') => return None,
                _ => {}
            }
        }
        Some(name)
    }

    /// Indented `name(...) {` / `name(...): T {` class and object methods.
    fn method(&self, mut i: usize) -> Option<Span> {
        let s = self.s;
        if matches!(s.get(i), Some(b'*' | b'#')) {
            i += 1;
        }
        let end = ident_end(s, i);
        if end == i || is_control(word(s, i, end)) {
            return None;
        }
        let mut k = skip_blank(s, end);
        if s.get(k) == Some(&b'<') {
            k = skip_blank(s, skip_balanced(s, k, b'<', b'>')?);
        }
        if s.get(k) != Some(&b'(') {
            return None;
        }
        let k = skip_blank(s, skip_balanced(s, k, b'(', b')')?);
        matches!(s.get(k), Some(b'{' | b':')).then_some((i, end))
    }
}

/// Detect import/use lines — lower value than definitions or usages.
///
/// Checks if the line (after leading whitespace) starts with a common
/// import statement prefix. Pure byte-level checks, no regex.
pub fn is_import_line(line: &str) -> bool {
    let s = line.trim_start().as_bytes();
    s.starts_with(b"import ")
        || s.starts_with(b"import\t")
        || (s.starts_with(b"from ") && s.get(5).is_some_and(|&b| b == b'\'' || b == b'"'))
        || s.starts_with(b"use ")
        || s.starts_with(b"use\t")
        || starts_with_require(s)
        || starts_with_include(s)
}

/// Match `require(` or `require (`.
#[inline]
fn starts_with_require(s: &[u8]) -> bool {
    if !s.starts_with(b"require") {
        return false;
    }
    let rest = &s[b"require".len()..];
    rest.first() == Some(&b'(') || (rest.first() == Some(&b' ') && rest.get(1) == Some(&b'('))
}

/// Match `# include ` (with optional spaces after `#`).
#[inline]
fn starts_with_include(s: &[u8]) -> bool {
    if s.first() != Some(&b'#') {
        return false;
    }
    let rest = &s[skip_blank(s, 1)..];
    rest.starts_with(b"include ") || rest.starts_with(b"include\t")
}

#[cfg(test)]
mod tests;
