macro_rules! declaration {
    ($language:ident, $attributes:ident, $fallback:ident, [$($modifier:literal),*], {$($($keyword:literal)|+ => $rule:ident),*}) => {
        impl super::parser::Grammar for super::parser::Parser<'_, { Language::$language as u8 }> {
            const LANGUAGE: Language = Language::$language;
            const ATTRIBUTES: Attributes = Attributes::$attributes;
            const FALLBACK: Fallback = Fallback::$fallback;

            #[inline(always)]
            fn is_modifier(word: &[u8]) -> bool {
                match word { $($modifier => true,)* _ => false }
            }

            #[inline(always)]
            fn is_declaration_prefix(word: &[u8]) -> bool {
                match word {
                    $($($keyword)|+ => super::rules::declaration_prefix!($rule),)*
                    _ => false,
                }
            }
        }

        impl<'a> super::parser::Parser<'a, { Language::$language as u8 }> {
            #[inline]
            fn is_definition_match(&mut self, ranges: &[(u32, u32)], fuzzy: bool) -> bool {
                let Some(name) = self.declaration() else {
                    return false;
                };
                if fuzzy {
                    return !ranges.is_empty() && ranges.iter().all(|&(start, end)| {
                        start < end && start as usize >= name.start && end as usize <= name.end
                    });
                }
                ranges.iter().any(|&(start, end)| {
                    start < end && (start as usize) < name.end && (end as usize) > name.start
                })
            }

            fn declaration(&mut self) -> Option<std::ops::Range<usize>> {
                self.whitespace();
                if Self::LANGUAGE == super::languages::Language::C {
                    if self.eat(b"#") {
                        let keyword = self.identifier()?;
                        return (&self.line[keyword] == b"define")
                            .then(|| self.identifier())
                            .flatten();
                    }
                    if self.eat(b"@") {
                        let keyword = self.identifier()?;
                        return matches!(&self.line[keyword], b"interface" | b"protocol")
                            .then(|| self.identifier())
                            .flatten();
                    }
                }
                self.attributes()?;
                #[allow(unused_mut)]
                let mut modifiers = false;
                loop {
                    if Self::LANGUAGE == super::languages::Language::Java && self.peek() == Some(b'<') {
                        self.group(b'<', b'>')?;
                    }
                    if Self::LANGUAGE == super::languages::Language::JavaScript {
                        self.eat(b"*");
                        self.eat(b"#");
                    }
                    let name = self.identifier()?;

                    match &self.line[name.clone()] {
                        $($modifier => super::rules::action!(Skip, self, name, modifiers),)*
                        $($($keyword)|+ => super::rules::action!($rule, self, name, modifiers),)*
                        _ => return self.fallback(name, modifiers),
                    }
                }
            }
        }
    };
}

macro_rules! action {
    (Reject, $p:ident, $name:ident, $modifiers:ident) => {{ return None }};
    (Skip, $p:ident, $name:ident, $modifiers:ident) => {{
        if Self::FALLBACK == super::languages::Fallback::Methods && $p.peek() == Some(b'(') {
            return $p.method($name);
        }
        $modifiers = true;
    }};
    (Visibility, $p:ident, $name:ident, $modifiers:ident) => {{
        if $p.peek() == Some(b'(') {
            $p.group(b'(', b')')?;
        }
        $modifiers = true;
    }};
    (Abi, $p:ident, $name:ident, $modifiers:ident) => {{
        if $p.peek() == Some(b'"') {
            $p.quoted()?;
        }
        $modifiers = true;
    }};
    (Template, $p:ident, $name:ident, $modifiers:ident) => {{
        $p.group(b'<', b'>')?;
        $modifiers = true;
    }};
    (RustConst, $p:ident, $name:ident, $modifiers:ident) => {{
        let $name = $p.identifier()?;
        if matches!(
            &$p.line[$name.clone()],
            b"fn" | b"unsafe" | b"async" | b"extern"
        ) {
            $p.pos = $name.start;
        } else {
            return Some($name);
        }
    }};
    (RustStatic, $p:ident, $name:ident, $modifiers:ident) => {{
        let $name = $p.identifier()?;
        return if &$p.line[$name.clone()] == b"mut" {
            $p.identifier()
        } else {
            Some($name)
        };
    }};
    (Macro, $p:ident, $name:ident, $modifiers:ident) => {{
        if !$p.eat(b"!") {
            return None;
        }
        return $p.identifier();
    }};
    (GoFunction, $p:ident, $name:ident, $modifiers:ident) => {{
        if $p.peek() == Some(b'(') {
            $p.group(b'(', b')')?;
        }
        return $p.identifier();
    }};
    (Function, $p:ident, $name:ident, $modifiers:ident) => {{
        $p.eat(b"*");
        $p.eat(b"&");
        return $p.identifier();
    }};
    (Generic, $p:ident, $name:ident, $modifiers:ident) => {{
        if $p.peek() == Some(b'<') {
            $p.group(b'<', b'>')?;
        }
        return $p.qualified_name();
    }};
    (Qualified, $p:ident, $name:ident, $modifiers:ident) => {{ return $p.qualified_name() }};
    (Name, $p:ident, $name:ident, $modifiers:ident) => {{
        if $p.skip_declaration_prefix() {
            continue;
        }
        return $p.declaration_name::<false, false>();
    }};
    (CType, $p:ident, $name:ident, $modifiers:ident) => {{
        if $p.skip_declaration_prefix() {
            continue;
        }
        if let Some(name) = $p.declaration_name::<true, false>() {
            return Some(name);
        }
        $p.pos = $name.start;
        return $p.c_function($modifiers);
    }};
    (MlType, $p:ident, $name:ident, $modifiers:ident) => {{
        if $p.skip_declaration_prefix() {
            continue;
        }
        return $p.declaration_name::<false, true>();
    }};
    (FunctionBinding, $p:ident, $name:ident, $modifiers:ident) => {{
        let $name = $p.identifier()?;
        $p.assignment()?;
        return Some($name);
    }};
    (ConstBinding, $p:ident, $name:ident, $modifiers:ident) => {{
        let $name = $p.identifier()?;
        if &$p.line[$name.clone()] == b"enum" {
            return $p.identifier();
        }
        $p.assignment()?;
        return Some($name);
    }};
    (Typedef, $p:ident, $name:ident, $modifiers:ident) => {{ return $p.typedef_name() }};
    (Alias, $p:ident, $name:ident, $modifiers:ident) => {{
        // `export * as Name from "module"` binds Name; anything else is an ordinary `as`.
        let start = $p.pos;
        if $modifiers
            && let Some(alias) = $p.identifier()
            && $p
                .identifier()
                .is_some_and(|from| &$p.line[from] == b"from")
        {
            return Some(alias);
        }
        $p.pos = start;
        return $p.fallback($name, $modifiers);
    }};
}

macro_rules! declaration_prefix {
    (Function) => {
        true
    };
    (CType) => {
        true
    };
    (MlType) => {
        true
    };
    ($other:ident) => {
        false
    };
}

pub(super) use {action, declaration, declaration_prefix};
