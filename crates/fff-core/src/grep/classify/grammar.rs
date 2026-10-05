//! Per-language definition grammars: which leading words are modifiers, which
//! keywords introduce a definition and how its name is read.

use super::DefinitionKind::{self, *};
use super::scan::kw;

/// Source language of a file, picked from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    Go,
    Python,
    /// JavaScript and TypeScript, including JSX/TSX and Vue/Svelte sources.
    JavaScript,
    Lua,
    /// C, C++, Objective-C and CUDA.
    C,
    /// Java, C#, Dart and Groovy: `Type name(...)` methods without a keyword.
    Java,
    Kotlin,
    Scala,
    Swift,
    Ruby,
    Elixir,
    Zig,
    Php,
    Shell,
    /// OCaml, ReasonML, ReScript and F#.
    Ml,
    Haskell,
    /// Prose and data files (Markdown, JSON, YAML, HTML...): never definitions.
    Text,
    /// Anything else: the union of the common keywords.
    Unknown,
}

impl Lang {
    /// Language of a file name (not a path), by extension.
    pub fn from_file_name(name: &str) -> Lang {
        let Some((_, ext)) = name.rsplit_once('.') else {
            return match name {
                "Rakefile" | "Gemfile" | "Podfile" | "Guardfile" => Lang::Ruby,
                _ => Lang::Unknown,
            };
        };
        let mut buf = [0u8; 8];
        if ext.len() > buf.len() {
            return Lang::Unknown;
        }
        buf[..ext.len()].copy_from_slice(ext.as_bytes());
        buf.make_ascii_lowercase();
        match &buf[..ext.len()] {
            b"rs" => Lang::Rust,
            b"go" => Lang::Go,
            b"py" | b"pyi" | b"pyw" => Lang::Python,
            b"js" | b"jsx" | b"mjs" | b"cjs" | b"ts" | b"tsx" | b"mts" | b"cts" | b"vue"
            | b"svelte" | b"astro" => Lang::JavaScript,
            b"lua" | b"luau" => Lang::Lua,
            b"c" | b"h" | b"cc" | b"cpp" | b"cxx" | b"c++" | b"hpp" | b"hh" | b"hxx" | b"h++"
            | b"ino" | b"m" | b"mm" | b"cu" | b"cuh" => Lang::C,
            b"java" | b"cs" | b"dart" | b"groovy" => Lang::Java,
            b"kt" | b"kts" => Lang::Kotlin,
            b"scala" | b"sc" | b"sbt" => Lang::Scala,
            b"swift" => Lang::Swift,
            b"rb" | b"rake" | b"gemspec" | b"ru" => Lang::Ruby,
            b"ex" | b"exs" => Lang::Elixir,
            b"zig" => Lang::Zig,
            b"php" => Lang::Php,
            b"sh" | b"bash" | b"zsh" | b"fish" | b"ksh" => Lang::Shell,
            b"ml" | b"mli" | b"re" | b"rei" | b"res" | b"resi" | b"fs" | b"fsi" | b"fsx" => {
                Lang::Ml
            }
            b"hs" => Lang::Haskell,
            b"md" | b"markdown" | b"mdx" | b"txt" | b"rst" | b"adoc" | b"org" | b"tex"
            | b"json" | b"jsonc" | b"json5" | b"yaml" | b"yml" | b"toml" | b"ini" | b"cfg"
            | b"conf" | b"env" | b"lock" | b"log" | b"csv" | b"tsv" | b"xml" | b"html" | b"htm"
            | b"svg" | b"css" | b"scss" | b"sass" | b"less" | b"sql" | b"diff" | b"patch" => {
                Lang::Text
            }
            _ => Lang::Unknown,
        }
    }
}

/// How the text after a definition keyword is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rule {
    /// `keyword name`.
    Named(DefinitionKind),
    /// `keyword <generics>? name`, Kotlin `fun <T> Foo.bar`.
    Generic(DefinitionKind),
    /// `keyword name` only at the top level (no indentation) or when exported:
    /// `const`/`let`/`var` would otherwise tag every local variable.
    TopLevel(DefinitionKind),
    /// A type keyword that C also uses in variable declarations and
    /// forward declarations: only a definition when followed by `{`, `:` or
    /// the end of the line.
    CType,
    /// Go `func (recv) name`.
    GoFunc,
    /// Rust `impl<T> Trait for Type`: everything up to `{`/`where` is the name.
    Impl,
    /// Rust `macro_rules! name`.
    MacroBang,
    /// C `typedef ... name;`.
    Typedef,
    /// Elixir `defstruct`: the keyword itself stands for the name.
    Bare(DefinitionKind),
}

/// Attribute syntax skipped before the declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Attributes {
    None,
    /// `#[...]` (Rust, PHP).
    Hash,
    /// `@Name(...)` (Java, Kotlin, Python, TypeScript, Swift, Scala).
    At,
    /// `@Name(...)` and `[Name]` (C#, shares the Java grammar).
    AtAndBrackets,
    /// `[[nodiscard]]` (C++).
    DoubleBrackets,
}

pub(super) struct Grammar {
    pub modifiers: &'static [u128],
    pub defs: &'static [(u128, Rule)],
    pub attributes: Attributes,
    /// `Type name(...)` functions and methods without a keyword (C, Java).
    pub c_like_functions: bool,
    /// Indented `name(...) {` class methods (JavaScript).
    pub methods: bool,
    /// `Foo.bar` / `Foo::bar` names (Ruby, Elixir, Lua, Kotlin...).
    pub path_names: bool,
    /// Lua `M:method` names.
    pub colon_paths: bool,
    /// Ruby/Elixir `name?` and `name!`.
    pub predicate_suffix: bool,
}

impl Grammar {
    #[inline]
    pub fn rule(&self, w: u128) -> Option<Rule> {
        self.defs.iter().find(|(k, _)| *k == w).map(|&(_, r)| r)
    }

    #[inline]
    pub fn is_modifier(&self, w: u128) -> bool {
        self.modifiers.contains(&w)
    }
}

macro_rules! words {
    ($($name:ident = $lit:literal),* $(,)?) => {
        $(pub(super) const $name: u128 = kw($lit);)*
    };
}

words! {
    ABSTRACT = b"abstract", ACCESSOR = b"accessor", ACTOR = b"actor", ANNOTATION = b"annotation",
    ASYNC = b"async", AUTO = b"auto", CASE = b"case", CLASS = b"class", COMPANION = b"companion",
    CONCEPT = b"concept", CONST = b"const", CONSTEVAL = b"consteval", CONSTEXPR = b"constexpr",
    CONSTINIT = b"constinit", CONVENIENCE = b"convenience", DATA = b"data", DECLARE = b"declare",
    DEF = b"def", DEFAULT = b"default", DEFDELEGATE = b"defdelegate", DEFEXCEPTION = b"defexception",
    DEFGUARD = b"defguard", DEFGUARDP = b"defguardp", DEFIMPL = b"defimpl", DEFMACRO = b"defmacro",
    DEFMACROP = b"defmacrop", DEFMODULE = b"defmodule", DEFN = b"defn", DEFP = b"defp",
    DEFPROTOCOL = b"defprotocol", DEFSTRUCT = b"defstruct", DYNAMIC = b"dynamic", ENUM = b"enum",
    EXPLICIT = b"explicit", EXPORT = b"export", EXTENSION = b"extension", EXTERN = b"extern",
    EXTERNAL = b"external", FILEPRIVATE = b"fileprivate", FINAL = b"final", FN = b"fn",
    FRIEND = b"friend", FUN = b"fun", FUNC = b"func", FUNCTION = b"function", GET = b"get",
    IMPL = b"impl", IMPLICIT = b"implicit", INDIRECT = b"indirect", INFIX = b"infix",
    INLINE = b"inline", INNER = b"inner", INSTANCE = b"instance", INTERFACE = b"interface",
    INTERNAL = b"internal", LAZY = b"lazy", LET = b"let", LOCAL = b"local",
    MACRO_RULES = b"macro_rules", MOD = b"mod", MODULE = b"module", MUTATING = b"mutating",
    NAMESPACE = b"namespace", NATIVE = b"native", NEWTYPE = b"newtype",
    NOINLINE = b"noinline", NONISOLATED = b"nonisolated", NONMUTATING = b"nonmutating",
    OBJECT = b"object", OPAQUE = b"opaque", OPEN = b"open", OPERATOR = b"operator",
    OPTIONAL = b"optional", OVERRIDE = b"override", PARTIAL = b"partial", PRIVATE = b"private",
    PROTECTED = b"protected", PROTOCOL = b"protocol", PUB = b"pub", PUBLIC = b"public",
    READONLY = b"readonly", REC = b"rec", RECORD = b"record", REQUIRED = b"required",
    SEALED = b"sealed", SET = b"set", STATIC = b"static", STRICTFP = b"strictfp",
    STRUCT = b"struct", SUSPEND = b"suspend", SYNCHRONIZED = b"synchronized", TAILREC = b"tailrec",
    TEMPLATE = b"template", THREADLOCAL = b"threadlocal", TRAIT = b"trait",
    TRANSIENT = b"transient", TRANSPARENT = b"transparent", TYPE = b"type",
    TYPEALIAS = b"typealias", TYPEDEF = b"typedef", UNION = b"union", UNSAFE = b"unsafe",
    VAL = b"val", VALUE = b"value", VAR = b"var", VIRTUAL = b"virtual", VOLATILE = b"volatile",
    MUT = b"mut", PACKED = b"packed", DEFINE = b"define",
}

/// Words that start statements, never declarations; a `name(...)` line led
/// by one of these is control flow or a call.
const CONTROL: &[u128] = &[
    kw(b"if"),
    kw(b"else"),
    kw(b"for"),
    kw(b"foreach"),
    kw(b"while"),
    kw(b"do"),
    kw(b"switch"),
    kw(b"case"),
    kw(b"catch"),
    kw(b"try"),
    kw(b"return"),
    kw(b"throw"),
    kw(b"new"),
    kw(b"delete"),
    kw(b"sizeof"),
    kw(b"typeof"),
    kw(b"await"),
    kw(b"yield"),
    kw(b"goto"),
    kw(b"using"),
    kw(b"lock"),
    kw(b"fixed"),
    kw(b"when"),
    kw(b"match"),
    kw(b"with"),
    kw(b"assert"),
    kw(b"super"),
    kw(b"this"),
    kw(b"elif"),
    kw(b"unless"),
    kw(b"until"),
    kw(b"defer"),
    kw(b"go"),
    kw(b"select"),
    kw(b"import"),
    kw(b"from"),
    kw(b"static_assert"),
    kw(b"function"),
];

#[inline]
pub(super) fn is_control(w: u128) -> bool {
    CONTROL.contains(&w)
}

const fn grammar(
    modifiers: &'static [u128],
    defs: &'static [(u128, Rule)],
    attributes: Attributes,
) -> Grammar {
    Grammar {
        modifiers,
        defs,
        attributes,
        c_like_functions: false,
        methods: false,
        path_names: false,
        colon_paths: false,
        predicate_suffix: false,
    }
}

static RUST: Grammar = grammar(
    &[PUB, ASYNC, UNSAFE, EXTERN, DEFAULT, AUTO],
    &[
        (FN, Rule::Named(Function)),
        (STRUCT, Rule::Named(Type)),
        (ENUM, Rule::Named(Type)),
        (UNION, Rule::Named(Type)),
        (TRAIT, Rule::Named(Type)),
        (TYPE, Rule::Named(Type)),
        (MOD, Rule::Named(Module)),
        (CONST, Rule::Named(Constant)),
        (STATIC, Rule::Named(Constant)),
        (IMPL, Rule::Impl),
        (MACRO_RULES, Rule::MacroBang),
    ],
    Attributes::Hash,
);

static GO: Grammar = grammar(
    &[],
    &[
        (FUNC, Rule::GoFunc),
        (TYPE, Rule::Named(Type)),
        (VAR, Rule::TopLevel(Variable)),
        (CONST, Rule::TopLevel(Constant)),
    ],
    Attributes::None,
);

static PYTHON: Grammar = grammar(
    &[ASYNC],
    &[(DEF, Rule::Named(Function)), (CLASS, Rule::Named(Type))],
    Attributes::At,
);

static JAVASCRIPT: Grammar = Grammar {
    methods: true,
    ..grammar(
        &[
            EXPORT, DEFAULT, DECLARE, ASYNC, ABSTRACT, PUBLIC, PRIVATE, PROTECTED, STATIC,
            READONLY, OVERRIDE, GET, SET, ACCESSOR,
        ],
        &[
            (FUNCTION, Rule::Named(Function)),
            (CLASS, Rule::Named(Type)),
            (INTERFACE, Rule::Named(Type)),
            (TYPE, Rule::Named(Type)),
            (ENUM, Rule::Named(Type)),
            (NAMESPACE, Rule::Named(Module)),
            (MODULE, Rule::Named(Module)),
            (CONST, Rule::TopLevel(Constant)),
            (LET, Rule::TopLevel(Variable)),
            (VAR, Rule::TopLevel(Variable)),
        ],
        Attributes::At,
    )
};

static LUA: Grammar = Grammar {
    path_names: true,
    colon_paths: true,
    ..grammar(
        &[LOCAL],
        &[(FUNCTION, Rule::Named(Function))],
        Attributes::None,
    )
};

static C: Grammar = Grammar {
    c_like_functions: true,
    path_names: true,
    ..grammar(
        &[
            STATIC, INLINE, EXTERN, VIRTUAL, EXPLICIT, CONSTEXPR, CONSTEVAL, CONSTINIT, FRIEND,
            EXPORT, TEMPLATE,
        ],
        &[
            (STRUCT, Rule::CType),
            (CLASS, Rule::CType),
            (UNION, Rule::CType),
            (ENUM, Rule::CType),
            (NAMESPACE, Rule::Named(Module)),
            (CONCEPT, Rule::Named(Type)),
            (TYPEDEF, Rule::Typedef),
        ],
        Attributes::DoubleBrackets,
    )
};

static JAVA: Grammar = Grammar {
    c_like_functions: true,
    ..grammar(
        &[
            PUBLIC,
            PRIVATE,
            PROTECTED,
            INTERNAL,
            STATIC,
            FINAL,
            ABSTRACT,
            SEALED,
            VIRTUAL,
            OVERRIDE,
            ASYNC,
            READONLY,
            PARTIAL,
            UNSAFE,
            EXTERN,
            SYNCHRONIZED,
            NATIVE,
            TRANSIENT,
            VOLATILE,
            STRICTFP,
            DEFAULT,
            CONST,
            EXTERNAL,
            LATE,
            REQUIRED,
        ],
        &[
            (CLASS, Rule::Named(Type)),
            (INTERFACE, Rule::Named(Type)),
            (ENUM, Rule::Named(Type)),
            (RECORD, Rule::Named(Type)),
            (STRUCT, Rule::Named(Type)),
            (NAMESPACE, Rule::Named(Module)),
            (DEF, Rule::Named(Function)),
            (TYPEDEF, Rule::Named(Type)),
            (EXTENSION, Rule::Named(Impl)),
            (MIXIN, Rule::Named(Type)),
        ],
        Attributes::AtAndBrackets,
    )
};

words! { LATE = b"late", MIXIN = b"mixin" }

static KOTLIN: Grammar = Grammar {
    path_names: true,
    ..grammar(
        &[
            PUBLIC, PRIVATE, PROTECTED, INTERNAL, OPEN, ABSTRACT, FINAL, OVERRIDE, SEALED, DATA,
            ENUM, ANNOTATION, INNER, VALUE, INLINE, SUSPEND, TAILREC, OPERATOR, INFIX, EXTERNAL,
            COMPANION, CONST, LATEINIT, EXPECT, ACTUAL,
        ],
        &[
            (FUN, Rule::Generic(Function)),
            (CLASS, Rule::Named(Type)),
            (INTERFACE, Rule::Named(Type)),
            (OBJECT, Rule::Named(Type)),
            (TYPEALIAS, Rule::Named(Type)),
            (VAL, Rule::TopLevel(Constant)),
            (VAR, Rule::TopLevel(Variable)),
        ],
        Attributes::At,
    )
};

words! { LATEINIT = b"lateinit", EXPECT = b"expect", ACTUAL = b"actual" }

static SCALA: Grammar = Grammar {
    path_names: true,
    ..grammar(
        &[
            CASE,
            PRIVATE,
            PROTECTED,
            OVERRIDE,
            FINAL,
            SEALED,
            ABSTRACT,
            IMPLICIT,
            LAZY,
            INLINE,
            OPAQUE,
            TRANSPARENT,
            OPEN,
        ],
        &[
            (DEF, Rule::Named(Function)),
            (CLASS, Rule::Named(Type)),
            (TRAIT, Rule::Named(Type)),
            (OBJECT, Rule::Named(Type)),
            (TYPE, Rule::Named(Type)),
            (ENUM, Rule::Named(Type)),
            (VAL, Rule::TopLevel(Constant)),
            (VAR, Rule::TopLevel(Variable)),
        ],
        Attributes::At,
    )
};

static SWIFT: Grammar = Grammar {
    path_names: true,
    ..grammar(
        &[
            PUBLIC,
            PRIVATE,
            FILEPRIVATE,
            INTERNAL,
            OPEN,
            STATIC,
            FINAL,
            OVERRIDE,
            MUTATING,
            NONMUTATING,
            CONVENIENCE,
            REQUIRED,
            LAZY,
            INDIRECT,
            DYNAMIC,
            NONISOLATED,
            OPTIONAL,
        ],
        &[
            (FUNC, Rule::Named(Function)),
            (CLASS, Rule::Named(Type)),
            (STRUCT, Rule::Named(Type)),
            (ENUM, Rule::Named(Type)),
            (PROTOCOL, Rule::Named(Type)),
            (ACTOR, Rule::Named(Type)),
            (TYPEALIAS, Rule::Named(Type)),
            (EXTENSION, Rule::Named(Impl)),
            (LET, Rule::TopLevel(Constant)),
            (VAR, Rule::TopLevel(Variable)),
        ],
        Attributes::At,
    )
};

static RUBY: Grammar = Grammar {
    path_names: true,
    predicate_suffix: true,
    ..grammar(
        &[PRIVATE, PROTECTED, PUBLIC],
        &[
            (DEF, Rule::Named(Function)),
            (CLASS, Rule::Named(Type)),
            (MODULE, Rule::Named(Module)),
        ],
        Attributes::None,
    )
};

static ELIXIR: Grammar = Grammar {
    path_names: true,
    predicate_suffix: true,
    ..grammar(
        &[],
        &[
            (DEF, Rule::Named(Function)),
            (DEFP, Rule::Named(Function)),
            (DEFN, Rule::Named(Function)),
            (DEFMACRO, Rule::Named(Macro)),
            (DEFMACROP, Rule::Named(Macro)),
            (DEFGUARD, Rule::Named(Function)),
            (DEFGUARDP, Rule::Named(Function)),
            (DEFDELEGATE, Rule::Named(Function)),
            (DEFMODULE, Rule::Named(Module)),
            (DEFPROTOCOL, Rule::Named(Module)),
            (DEFIMPL, Rule::Named(Impl)),
            (DEFSTRUCT, Rule::Bare(Type)),
            (DEFEXCEPTION, Rule::Bare(Type)),
        ],
        Attributes::None,
    )
};

static ZIG: Grammar = grammar(
    &[PUB, EXPORT, EXTERN, INLINE, NOINLINE, THREADLOCAL],
    &[
        (FN, Rule::Named(Function)),
        (CONST, Rule::TopLevel(Constant)),
        (VAR, Rule::TopLevel(Variable)),
    ],
    Attributes::None,
);

static PHP: Grammar = grammar(
    &[
        PUBLIC, PRIVATE, PROTECTED, STATIC, ABSTRACT, FINAL, READONLY,
    ],
    &[
        (FUNCTION, Rule::Named(Function)),
        (CLASS, Rule::Named(Type)),
        (INTERFACE, Rule::Named(Type)),
        (TRAIT, Rule::Named(Type)),
        (ENUM, Rule::Named(Type)),
    ],
    Attributes::Hash,
);

static SHELL: Grammar = grammar(&[], &[(FUNCTION, Rule::Named(Function))], Attributes::None);

static ML: Grammar = Grammar {
    path_names: true,
    ..grammar(
        &[REC, PRIVATE, INLINE],
        &[
            (LET, Rule::TopLevel(Function)),
            (TYPE, Rule::Named(Type)),
            (MODULE, Rule::Named(Module)),
            (EXTERNAL, Rule::Named(Function)),
            (VAL, Rule::TopLevel(Function)),
            (EXCEPTION, Rule::Named(Type)),
        ],
        Attributes::None,
    )
};

words! { EXCEPTION = b"exception" }

static HASKELL: Grammar = grammar(
    &[],
    &[
        (DATA, Rule::Named(Type)),
        (NEWTYPE, Rule::Named(Type)),
        (TYPE, Rule::Named(Type)),
        (CLASS, Rule::Named(Type)),
        (INSTANCE, Rule::Named(Impl)),
        (MODULE, Rule::Named(Module)),
    ],
    Attributes::None,
);

static TEXT: Grammar = grammar(&[], &[], Attributes::None);

static UNKNOWN: Grammar = grammar(
    &[
        PUB, EXPORT, DEFAULT, ASYNC, ABSTRACT, UNSAFE, STATIC, PROTECTED, PRIVATE, PUBLIC,
    ],
    &[
        (STRUCT, Rule::Named(Type)),
        (FN, Rule::Named(Function)),
        (ENUM, Rule::Named(Type)),
        (TRAIT, Rule::Named(Type)),
        (IMPL, Rule::Impl),
        (CLASS, Rule::Named(Type)),
        (INTERFACE, Rule::Named(Type)),
        (FUNCTION, Rule::Named(Function)),
        (DEF, Rule::Named(Function)),
        (FUNC, Rule::Named(Function)),
        (FUN, Rule::Named(Function)),
        (TYPE, Rule::Named(Type)),
        (MODULE, Rule::Named(Module)),
        (OBJECT, Rule::Named(Type)),
    ],
    Attributes::None,
);

impl Lang {
    pub(super) fn grammar(self) -> &'static Grammar {
        match self {
            Lang::Rust => &RUST,
            Lang::Go => &GO,
            Lang::Python => &PYTHON,
            Lang::JavaScript => &JAVASCRIPT,
            Lang::Lua => &LUA,
            Lang::C => &C,
            Lang::Java => &JAVA,
            Lang::Kotlin => &KOTLIN,
            Lang::Scala => &SCALA,
            Lang::Swift => &SWIFT,
            Lang::Ruby => &RUBY,
            Lang::Elixir => &ELIXIR,
            Lang::Zig => &ZIG,
            Lang::Php => &PHP,
            Lang::Shell => &SHELL,
            Lang::Ml => &ML,
            Lang::Haskell => &HASKELL,
            Lang::Text => &TEXT,
            Lang::Unknown => &UNKNOWN,
        }
    }
}
