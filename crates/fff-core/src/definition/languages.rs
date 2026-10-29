use super::parser::Grammar;

#[derive(Clone, Copy)]
pub(super) enum Attributes {
    None,
    Hash,
    At,
    AtAndBrackets,
    DoubleBrackets,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Fallback {
    None,
    Methods,
    Assignment,
    CFunction,
}

macro_rules! languages {
    ($($language:ident {
        extensions: [$($extension:literal),+ $(,)?],
        attributes: $attributes:ident,
        fallback: $fallback:ident,
        modifiers: [$($modifier:literal),* $(,)?],
        rules: { $($($keyword:literal)|+ => $rule:ident),* $(,)? }
    }),+ $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub(crate) enum Language { Unsupported, $($language),+ }

        const EXTENSIONS: &[(&[u8], Language)] = &[
            $($(($extension, Language::$language),)+)+
        ];
        static SUFFIXES: super::suffix::SuffixMatcher<{ EXTENSIONS.len() }> =
            super::suffix::SuffixMatcher::new(EXTENSIONS);

        impl Language {
            #[inline]
            pub(crate) fn from_filename(filename: &str) -> Self {
                SUFFIXES.classify(filename.as_bytes())
            }
        }

        #[inline]
        pub(crate) fn is_definition_match(
            language: u8,
            line: &[u8],
            ranges: &[(u32, u32)],
            fuzzy: bool,
        ) -> bool {
            if line.len() > 256 {
                return false;
            }
            classify(language, line, ranges, fuzzy)
        }

        // Keep the parser's stack frame off the long-line rejection path.
        #[inline(never)]
        fn classify(language: u8, line: &[u8], ranges: &[(u32, u32)], fuzzy: bool) -> bool {
            match language {
                $(id if id == Language::$language as u8 => {
                    super::parser::Parser::<{ Language::$language as u8 }>::new(line)
                        .is_definition_match(ranges, fuzzy)
                },)+
                _ => false,
            }
        }

        $(super::rules::declaration!($language, $attributes, $fallback, [$($modifier),*], {$($($keyword)|+ => $rule),*});)+

        const _: () = { $(assert!(
            Language::$language as u8 <= u8::MAX >> crate::types::FileItemFlags::LANGUAGE_SHIFT
        );)+ };
    };
}

languages! {
    Rust {
        extensions: [b"rs"],
        attributes: Hash,
        fallback: None,
        modifiers: [b"async", b"unsafe", b"default"],
        rules: {
            b"pub" => Visibility, b"extern" => Abi,
            b"const" => RustConst, b"static" => RustStatic,
            b"fn" => Function,
            b"mod" | b"struct" | b"enum" | b"trait" | b"type" | b"union" | b"macro" => Name,
            b"macro_rules" => Macro, b"impl" => Reject
        }
    },
    Python {
        extensions: [b"py", b"pyi", b"pyw"],
        attributes: At,
        fallback: None,
        modifiers: [b"async"],
        rules: { b"def" => Function, b"class" => Name }
    },
    Go {
        extensions: [b"go"],
        attributes: None,
        fallback: None,
        modifiers: [],
        rules: { b"func" => GoFunction, b"type" | b"const" | b"var" => Name }
    },
    JavaScript {
        extensions: [
            b"js", b"jsx", b"mjs", b"cjs", b"ts", b"tsx", b"mts", b"cts", b"vue", b"svelte",
            b"astro"
        ],
        attributes: At,
        fallback: Methods,
        modifiers: [
            b"export", b"default", b"declare", b"abstract", b"public", b"private", b"protected",
            b"static", b"override", b"async", b"readonly", b"get", b"set"
        ],
        rules: {
            b"function" => Function,
            b"class" | b"interface" | b"type" | b"enum" | b"namespace" | b"module" => Name,
            b"const" => ConstBinding, b"let" | b"var" => FunctionBinding, b"as" => Alias
        }
    },
    Lua {
        extensions: [b"lua", b"luau"],
        attributes: None,
        fallback: Assignment,
        modifiers: [b"local"],
        rules: { b"function" => Qualified }
    },
    C {
        extensions: [
            b"c", b"h", b"cc", b"cpp", b"cxx", b"c++", b"hpp", b"hh", b"hxx", b"h++", b"ino", b"m",
            b"mm", b"cu", b"cuh"
        ],
        attributes: DoubleBrackets,
        fallback: CFunction,
        modifiers: [
            b"static", b"inline", b"virtual", b"explicit", b"constexpr", b"consteval", b"constinit",
            b"friend", b"export"
        ],
        rules: {
            b"extern" => Abi, b"template" => Template,
            b"struct" | b"class" | b"enum" | b"union" => CType,
            b"namespace" => Qualified, b"concept" => Name, b"typedef" => Typedef
        }
    },
    Java {
        extensions: [b"java", b"cs", b"dart", b"groovy"],
        attributes: AtAndBrackets,
        fallback: CFunction,
        modifiers: [
            b"public", b"private", b"protected", b"internal", b"static", b"final", b"abstract",
            b"sealed", b"virtual", b"override", b"async", b"readonly", b"partial", b"unsafe",
            b"extern", b"synchronized", b"native", b"transient", b"volatile", b"strictfp",
            b"default", b"const", b"external", b"late", b"required"
        ],
        rules: {
            b"class" | b"interface" | b"enum" | b"record" | b"struct" | b"mixin" | b"typedef" => Name,
            b"namespace" => Qualified, b"def" => Function, b"extension" => Reject
        }
    },
    Kotlin {
        extensions: [b"kt", b"kts"],
        attributes: At,
        fallback: None,
        modifiers: [
            b"public", b"private", b"protected", b"internal", b"open", b"abstract", b"final",
            b"override", b"sealed", b"data", b"enum", b"annotation", b"inner", b"value", b"inline",
            b"suspend", b"tailrec", b"operator", b"infix", b"external", b"companion", b"const",
            b"lateinit", b"expect", b"actual"
        ],
        rules: {
            b"fun" => Generic,
            b"class" | b"interface" | b"object" | b"typealias" | b"val" | b"var" => Name
        }
    },
    Scala {
        extensions: [b"scala", b"sc", b"sbt"],
        attributes: At,
        fallback: None,
        modifiers: [
            b"case", b"private", b"protected", b"override", b"final", b"sealed", b"abstract",
            b"implicit", b"lazy", b"inline", b"opaque", b"transparent", b"open"
        ],
        rules: {
            b"def" => Qualified,
            b"class" | b"trait" | b"object" | b"type" | b"enum" | b"val" | b"var" => Name,
            b"extension" => Reject
        }
    },
    Swift {
        extensions: [b"swift"],
        attributes: At,
        fallback: None,
        modifiers: [
            b"public", b"private", b"fileprivate", b"internal", b"open", b"static", b"final",
            b"override", b"mutating", b"nonmutating", b"convenience", b"required", b"lazy",
            b"indirect", b"dynamic", b"nonisolated", b"optional"
        ],
        rules: {
            b"func" => Function,
            b"class" | b"struct" | b"enum" | b"protocol" | b"actor" | b"typealias" | b"let" | b"var" => Name,
            b"extension" => Reject
        }
    },
    Ruby {
        extensions: [b"rb", b"rake", b"gemspec", b"ru"],
        attributes: None,
        fallback: None,
        modifiers: [b"private", b"protected", b"public"],
        rules: { b"def" | b"class" | b"module" => Qualified }
    },
    Zig {
        extensions: [b"zig"],
        attributes: None,
        fallback: None,
        modifiers: [b"pub", b"export", b"inline", b"noinline", b"threadlocal"],
        rules: { b"extern" => Abi, b"fn" => Function, b"const" | b"var" => Name }
    },
    Ml {
        extensions: [b"ml", b"mli", b"re", b"rei", b"res", b"resi", b"fs", b"fsi", b"fsx"],
        attributes: None,
        fallback: None,
        modifiers: [b"rec", b"private", b"inline", b"mutable"],
        rules: {
            b"let" | b"module" | b"external" | b"val" | b"exception" => Name,
            b"type" => MlType
        }
    },
}

#[cfg(test)]
mod tests {
    use super::{EXTENSIONS, Language};

    #[test]
    fn classifies_registered_suffixes() {
        for &(extension, language) in EXTENSIONS {
            let extension = std::str::from_utf8(extension).unwrap();
            for prefix in [
                "",
                "a",
                "long_filename",
                "directory.with.dots/",
                "目录/文件",
            ] {
                for extension in [extension.to_string(), extension.to_ascii_uppercase()] {
                    let filename = format!("{prefix}.{extension}");
                    assert!(Language::from_filename(&filename) == language, "{filename}");
                }
            }
        }
    }

    #[test]
    fn rejects_non_suffixes_and_bucket_collisions() {
        for filename in [
            "",
            "rs",
            "x.",
            "Gemfile",
            "Rakefile",
            "Podfile",
            "Guardfile",
            "a.rs.bak",
            "a.svelte.bak",
            "dir.rs/no_extension",
            "dir.rs\\no_extension",
            "a.rs/",
            "a.rs\\",
            "a\x0eRS",
            "a.c\x0b\x0b",
            "a.ſh",
            "a.extensiontoolong",
        ] {
            assert!(
                Language::from_filename(filename) == Language::Unsupported,
                "{filename:?}"
            );
        }
    }
}
