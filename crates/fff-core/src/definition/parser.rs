use super::languages::{Attributes, Fallback, Language};
use std::ops::Range;

pub(super) struct Parser<'a, const LANGUAGE: u8> {
    pub(super) line: &'a [u8],
    pub(super) pos: usize,
}

// Each generated checker supplies compile-time grammar constants and keyword predicates.
pub(super) trait Grammar {
    const LANGUAGE: Language;
    const ATTRIBUTES: Attributes;
    const FALLBACK: Fallback;
    fn is_modifier(word: &[u8]) -> bool;
    fn is_declaration_prefix(word: &[u8]) -> bool;
}

impl<'a, const LANGUAGE: u8> Parser<'a, LANGUAGE>
where
    Self: Grammar,
{
    pub(super) fn new(line: &'a [u8]) -> Self {
        Self { line, pos: 0 }
    }

    pub(super) fn skip_declaration_prefix(&mut self) -> bool {
        if !matches!(Self::LANGUAGE, Language::C | Language::Swift | Language::Ml) {
            return false;
        }
        let start = self.pos;
        let prefix = self.identifier().is_some_and(|next| {
            let word = &self.line[next];
            Self::is_declaration_prefix(word)
                || (Self::LANGUAGE == Language::Swift && word == b"var")
        });
        self.pos = start;
        prefix
    }

    pub(super) fn declaration_name<const C_TYPE: bool, const ML_TYPE: bool>(
        &mut self,
    ) -> Option<Range<usize>> {
        if ML_TYPE {
            while self.eat(b"'") {
                self.identifier()?;
            }
            if self.peek() == Some(b'(') {
                self.group(b'(', b')')?;
            }
        }
        let mut name = self.named()?;
        if C_TYPE {
            // `class BASE_EXPORT Name` and `struct COMPONENT_EXPORT(NET) Name` put a macro first.
            while is_macro_case(&self.line[name.clone()]) {
                let start = self.pos;
                if self.peek() == Some(b'(') {
                    self.group(b'(', b')')?;
                }
                match self.identifier() {
                    Some(next) if &self.line[next.clone()] != b"final" => name = next,
                    _ => {
                        self.pos = start;
                        break;
                    }
                }
            }
            let defines = matches!(self.peek(), None | Some(b'{' | b':' | b'<'))
                || self
                    .identifier()
                    .is_some_and(|word| &self.line[word] == b"final");
            return defines.then_some(name);
        }
        Some(name)
    }

    fn named(&mut self) -> Option<Range<usize>> {
        loop {
            let name = self.identifier()?;
            if Self::LANGUAGE != Language::Ml || !Self::is_modifier(&self.line[name.clone()]) {
                return Some(name);
            }
        }
    }

    pub(super) fn qualified_name(&mut self) -> Option<Range<usize>> {
        let mut name = self.identifier()?;
        loop {
            let start = self.pos;
            if Self::LANGUAGE == Language::Kotlin && self.peek() == Some(b'<') {
                if self.group(b'<', b'>').is_none() || !self.eat(b".") {
                    self.pos = start;
                    break;
                }
            } else if !(self.eat(b".")
                || self.eat(b"::")
                || (Self::LANGUAGE == Language::Lua && self.eat(b":")))
            {
                break;
            }
            name = self.identifier()?;
        }
        if Self::LANGUAGE == Language::Ruby && matches!(self.line.get(self.pos), Some(b'?' | b'!'))
        {
            self.pos += 1;
            name.end = self.pos;
        }
        Some(name)
    }

    pub(super) fn attributes(&mut self) -> Option<()> {
        loop {
            self.whitespace();
            let tail = &self.line[self.pos..];
            match Self::ATTRIBUTES {
                Attributes::Hash if tail.starts_with(b"#[") || tail.starts_with(b"#![") => {
                    self.pos += if tail.starts_with(b"#!") { 2 } else { 1 };
                    self.group(b'[', b']')?;
                }
                Attributes::At | Attributes::AtAndBrackets if tail.starts_with(b"@") => {
                    self.pos += 1;
                    self.path_name()?;
                    if self.peek() == Some(b'(') {
                        self.group(b'(', b')')?;
                    }
                }
                Attributes::AtAndBrackets if tail.starts_with(b"[") => {
                    self.group(b'[', b']')?;
                }
                Attributes::DoubleBrackets if tail.starts_with(b"[[") => {
                    self.group(b'[', b']')?;
                }
                _ => return Some(()),
            }
        }
    }

    pub(super) fn fallback(&mut self, name: Range<usize>, modifiers: bool) -> Option<Range<usize>> {
        if matches!(
            &self.line[name.clone()],
            b"return"
                | b"throw"
                | b"new"
                | b"if"
                | b"else"
                | b"for"
                | b"foreach"
                | b"while"
                | b"do"
                | b"switch"
                | b"case"
                | b"catch"
                | b"try"
                | b"with"
                | b"await"
                | b"yield"
                | b"delete"
                | b"sizeof"
                | b"typeof"
                | b"assert"
                | b"using"
                | b"lock"
                | b"match"
        ) {
            return None;
        }
        match Self::FALLBACK {
            Fallback::None => None,
            Fallback::Methods => self.method(name),
            Fallback::Assignment => {
                let name = self.path_tail(name)?;
                self.assignment()?;
                Some(name)
            }
            Fallback::CFunction => {
                self.pos = name.start;
                self.c_function(modifiers)
            }
        }
    }

    pub(super) fn method(&mut self, name: Range<usize>) -> Option<Range<usize>> {
        self.eat(b"?");
        self.eat(b"!");
        if matches!(self.peek(), Some(b'=' | b':')) {
            self.assignment()?;
            return Some(name);
        }
        if self.peek() == Some(b'<') {
            self.group(b'<', b'>')?;
        }
        self.group(b'(', b')')?;
        matches!(self.peek(), Some(b'{' | b':')).then_some(name)
    }

    pub(super) fn c_function(&mut self, modifiers: bool) -> Option<Range<usize>> {
        let mut names = 0;
        let mut last: Option<Range<usize>> = None;
        let mut qualified = false;
        let mut constructor = false;
        loop {
            if let Some(name) = self.identifier() {
                constructor = qualified
                    && last
                        .as_ref()
                        .is_some_and(|parent| self.line[parent.clone()] == self.line[name.clone()]);
                if !qualified {
                    names += 1;
                }
                qualified = false;
                last = Some(name);
            } else {
                match self.peek()? {
                    b'(' => break,
                    b'*' | b'&' | b'~' | b'?' | b'^' => self.pos += 1,
                    b':' if self.eat(b"::") => qualified = true,
                    b'<' => self.group(b'<', b'>')?,
                    b'[' => self.group(b'[', b']')?,
                    _ => return None,
                }
            }
        }
        let name = last?;
        if names < 2 && !modifiers && !constructor
            || Self::LANGUAGE == Language::C
                && matches!(
                    &self.line[name.clone()],
                    b"sizeof"
                        | b"alignof"
                        | b"decltype"
                        | b"typeof"
                        | b"__attribute__"
                        | b"__declspec"
                        | b"alignas"
                        | b"asm"
                        | b"__asm__"
                        | b"volatile"
                )
        {
            return None;
        }
        // An unfinished parameter list can continue on the following line.
        if self.group(b'(', b')').is_some() {
            match self.peek() {
                Some(b',' | b')' | b'.') => return None,
                // Accept unindented prototypes and virtual/explicit members;
                // skip ambiguous indented declarations.
                Some(b';')
                    if Self::LANGUAGE == Language::C
                        && (!(self.line.first().is_some_and(|b| !b.is_ascii_whitespace())
                            || matches!(first_word(self.line), b"virtual" | b"explicit"))
                            || is_macro_case(&self.line[name.clone()])) =>
                {
                    return None;
                }
                Some(b'=') if !self.line[self.pos..].starts_with(b"=>") => return None,
                _ => {}
            }
        }
        Some(name)
    }

    pub(super) fn typedef_name(&mut self) -> Option<Range<usize>> {
        if let Some(pointer) = memchr::memmem::find(&self.line[self.pos..], b"(*") {
            self.pos += pointer + 2;
            return self.identifier();
        }
        let start = self.pos;
        let tag = self
            .identifier()
            .filter(|word| matches!(&self.line[word.clone()], b"struct" | b"union" | b"enum"))
            .and_then(|_| {
                let mut tag = self.identifier()?;
                // `typedef struct API_AVAILABLE(macos(10.11)) Name {` puts an attribute first.
                while self.peek() == Some(b'(') {
                    self.group(b'(', b')')?;
                    tag = self.identifier()?;
                }
                Some(tag)
            });
        if tag.is_none() {
            self.pos = start;
        }
        let mut last = tag.clone();
        while self.peek().is_some() {
            match self.peek()? {
                b';' => break,
                b'[' => self.group(b'[', b']')?,
                // A body that continues on later lines leaves the tag as the name.
                b'{' if self.group(b'{', b'}').is_none() => return tag,
                b'{' => {}
                _ => {
                    if let Some(name) = self.identifier() {
                        last = Some(name);
                    } else {
                        self.pos += 1;
                    }
                }
            }
        }
        last
    }

    pub(super) fn assignment(&mut self) -> Option<()> {
        if self.eat(b":") {
            // Skip a TypeScript annotation, stopping before its initializer.
            while !matches!(self.peek(), None | Some(b';')) {
                match self.peek()? {
                    b'=' if self.eat(b"=>") => {}
                    b'=' => break,
                    b'(' => self.group(b'(', b')')?,
                    b'<' => self.group(b'<', b'>')?,
                    b'{' => self.group(b'{', b'}')?,
                    b'[' => self.group(b'[', b']')?,
                    _ => self.pos += 1,
                }
            }
        }
        if !self.eat(b"=") || self.peek() == Some(b'=') {
            return None;
        }
        let start = self.pos;
        if let Some(name) = self.identifier() {
            match &self.line[name] {
                b"function" => return Some(()),
                b"class" if Self::LANGUAGE == Language::JavaScript => return Some(()),
                b"async" if Self::LANGUAGE == Language::JavaScript => {
                    if let Some(name) = self.identifier() {
                        return (&self.line[name] == b"function" || self.eat(b"=>")).then_some(());
                    }
                }
                _ if Self::LANGUAGE == Language::JavaScript => {
                    return self.eat(b"=>").then_some(());
                }
                _ => return None,
            }
        } else {
            self.pos = start;
        }
        if Self::LANGUAGE != Language::JavaScript {
            return None;
        }
        if self.peek() == Some(b'<') {
            self.group(b'<', b'>')?;
        }
        if self.peek() == Some(b'(') {
            self.group(b'(', b')')?;
        } else {
            self.identifier()?;
        }
        if self.eat(b":") {
            let arrow = memchr::memmem::find(&self.line[self.pos..], b"=>")?;
            self.pos += arrow;
        }
        self.eat(b"=>").then_some(())
    }

    fn path_name(&mut self) -> Option<std::ops::Range<usize>> {
        let name = self.identifier()?;
        self.path_tail(name)
    }

    fn path_tail(&mut self, mut name: std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
        while self.eat(b".") || self.eat(b"::") || self.eat(b":") {
            name = self.identifier()?;
        }
        Some(name)
    }

    #[inline(always)]
    pub(super) fn identifier(&mut self) -> Option<std::ops::Range<usize>> {
        self.whitespace();
        if self.line[self.pos..].starts_with(b"r#") {
            self.pos += 2;
        }
        let start = self.pos;
        if !self
            .line
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b'$' || *b >= 128)
        {
            return None;
        }
        self.pos = super::scan::identifier_end(self.line, self.pos + 1);
        Some(start..self.pos)
    }

    pub(super) fn group(&mut self, open: u8, close: u8) -> Option<()> {
        self.whitespace();
        if self.line.get(self.pos) != Some(&open) {
            return None;
        }
        self.pos += 1;
        let mut depth = 1;
        while depth > 0 {
            let tail = &self.line[self.pos..];
            let bracket = memchr::memchr2(open, close, tail)?;
            let quote = memchr::memchr3(b'"', b'\'', b'`', &tail[..bracket]);
            if let Some(quote) = quote {
                self.pos += quote;
                // Rust lifetimes inside generics are identifiers, not strings.
                if open == b'<' && self.line[self.pos] == b'\'' {
                    self.pos += 1;
                    continue;
                }
                self.quoted()?;
            } else {
                self.pos += bracket;
                if self.line[self.pos] == open {
                    depth += 1;
                } else if close != b'>' || !matches!(self.line[self.pos - 1], b'-' | b'=') {
                    depth -= 1;
                }
                self.pos += 1;
            }
        }
        Some(())
    }

    pub(super) fn quoted(&mut self) -> Option<()> {
        let quote = self.line[self.pos];
        self.pos += 1;
        loop {
            self.pos += memchr::memchr2(quote, b'\\', &self.line[self.pos..])?;
            let byte = self.line[self.pos];
            self.pos += 1;
            if byte == quote {
                return Some(());
            }
            if self.pos == self.line.len() {
                return None;
            }
            self.pos += 1;
        }
    }

    pub(super) fn eat(&mut self, token: &[u8]) -> bool {
        self.whitespace();
        if !self.line[self.pos..].starts_with(token) {
            return false;
        }
        self.pos += token.len();
        true
    }

    pub(super) fn peek(&mut self) -> Option<u8> {
        self.whitespace();
        self.line.get(self.pos).copied()
    }

    pub(super) fn whitespace(&mut self) {
        while self.line.get(self.pos).is_some_and(u8::is_ascii_whitespace) {
            self.pos += 1;
        }
    }
}

fn is_macro_case(word: &[u8]) -> bool {
    word.iter().any(u8::is_ascii_uppercase)
        && word
            .iter()
            .all(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn first_word(line: &[u8]) -> &[u8] {
    let start = line
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(line.len());
    &line[start..super::scan::identifier_end(line, start)]
}
