mod languages;
mod parser;
mod rules;
mod scan;
mod suffix;

pub(crate) use languages::{Language, is_definition_match};

#[cfg(test)]
mod tests {
    use super::{Language, is_definition_match};

    #[test]
    fn generated_rules_classify_declared_names() {
        for (filename, line, expected) in [
            ("names.rs", "#[derive(Clone)] pub struct user_file;", true),
            ("names.rs", "impl user_file {}", false),
            ("names.py", "class user_file(Base):", true),
            ("names.py", "def other(value: user_file):", false),
            ("names.go", "type user_file struct {}", true),
            ("names.go", "func (s *user_file) other() {}", false),
            ("names.ts", "export class user_file extends Base {}", true),
            ("names.ts", "export const enum user_file { Ready }", true),
            ("names.ts", "const current: user_file = Mode.Ready;", false),
            ("names.ts", "function other(value: user_file) {}", false),
            ("names.lua", "local function user_file() end", true),
            ("names.lua", "function user_file.other() end", false),
            ("names.cpp", "struct user_file {", true),
            ("names.cpp", "struct Packet *user_file(void) {", true),
            ("names.cpp", "struct user_file *create(void) {", false),
            ("names.cpp", "struct user_file instance;", false),
            ("names.java", "public class user_file extends Base {", true),
            (
                "names.java",
                "public static <T> List<T> user_file(T... xs) {",
                true,
            ),
            (
                "names.java",
                "public static <user_file> List<user_file> create() {",
                false,
            ),
            ("names.java", "public user_file other() {", false),
            ("names.kt", "data class user_file(val x: Int)", true),
            ("names.kt", "fun user_file.other() {}", false),
            ("names.scala", "case class user_file(x: Int)", true),
            ("names.scala", "extension (value: user_file)", false),
            ("names.swift", "public struct user_file {", true),
            ("names.swift", "extension user_file {", false),
            ("names.rb", "def self.user_file?", true),
            ("names.rb", "def user_file.other", false),
            ("names.ex", "defp user_file?(arg) do", true),
            ("names.ex", "defimpl user_file, for: Other do", false),
            ("names.zig", "pub const user_file = struct {", true),
            ("names.zig", "fn other(value: user_file) void {", false),
            ("names.php", "#[Attribute] class user_file {", true),
            ("names.php", "class Other extends user_file {", false),
            ("names.sh", "user_file() {", true),
            ("names.sh", "echo user_file", false),
            ("names.ml", "type 'a user_file = 'a list", true),
            ("names.ml", "let other = user_file value", false),
            ("names.hs", "user_file :: Int -> Int", true),
        ] {
            let start = line.find("user_file").unwrap() as u32;
            let ranges = [(start, start + 9)];
            let language = Language::from_filename(filename) as u8;
            for fuzzy in [false, true] {
                assert_eq!(
                    is_definition_match(language, line.as_bytes(), &ranges, fuzzy),
                    expected,
                    "{filename}: {line}"
                );
                if expected {
                    for length in [255, 256, 257, 1024] {
                        let padded = format!("{line:<length$}");
                        assert_eq!(
                            is_definition_match(language, padded.as_bytes(), &ranges, fuzzy),
                            length <= 256,
                            "{filename}: {length} bytes, fuzzy={fuzzy}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fuzzy_definition_ranges_stay_inside_the_name() {
        let language = Language::from_filename("file.rs") as u8;
        let line = b"fn user(file: u8) {}";
        assert!(is_definition_match(language, line, &[(3, 7)], true));
        assert!(!is_definition_match(
            language,
            line,
            &[(3, 7), (8, 12)],
            true
        ));
        assert!(!is_definition_match(language, line, &[], true));
        assert!(!is_definition_match(language, line, &[(3, 3)], true));
        assert!(!is_definition_match(255, line, &[(3, 7)], true));
    }

    #[test]
    fn matches_must_start_at_the_declared_name() {
        for (filename, line, needle, expected) in [
            ("x.rs", "pub enum Value {", "Value", true),
            ("x.rs", "pub enum ValueKind {", "Value", true),
            ("x.rs", "pub enum Value {", "enum Value", true),
            (
                "x.rs",
                "pub struct ArgValueCompletion<'a> {",
                "Value",
                false,
            ),
            ("x.rs", "pub trait FromValue: Sized {", "Value", false),
            ("x.rs", "fn value_of(v: Value) -> i64 {", "Value", false),
            ("x.rs", "pub fn r#type() {}", "r#type", true),
            (
                "x.ts",
                "export class UserController {",
                "UserController",
                true,
            ),
            (
                "x.ts",
                "export class AdminUserController {",
                "UserController",
                false,
            ),
            (
                "x.ts",
                "function handle(ctrl: UserController) {",
                "UserController",
                false,
            ),
            ("x.ts", "  #load() {", "#load", true),
            ("x.lua", "function M.setup(opts)", "M.setup", true),
            ("x.lua", "function M.setup(opts)", "setup", true),
            ("x.zig", "pub const PageList = @This();", "Page", true),
            ("x.zig", "fn getTabPage(self: *Self) void {", "Page", false),
            ("x.h", "typedef struct AVFrame {", "AVFrame", true),
            ("x.h", "struct _xmlDict {", "xmlDict", true),
            ("x.h", "struct _xmlDict {", "_xmlDict", true),
            ("x.h", "struct _xmlDict {", "Dict", false),
            ("x.py", "def __init__(self):", "init", true),
            ("x.h", "typedef struct MyAVPacketList {", "AVPacket", false),
            (
                "x.h",
                "class BASE_EXPORT CommandLine {",
                "BASE_EXPORT CommandLine",
                true,
            ),
        ] {
            let start = line.find(needle).unwrap() as u32;
            let ranges = [(start, start + needle.len() as u32)];
            let language = Language::from_filename(filename) as u8;
            assert_eq!(
                is_definition_match(language, line.as_bytes(), &ranges, false),
                expected,
                "{filename}: {needle} in {line}"
            );
        }
        // Typo matching still accepts characters anywhere inside the name.
        let line = b"pub struct ArgValueCompletion<'a> {";
        assert!(is_definition_match(
            Language::Rust as u8,
            line,
            &[(14, 19)],
            true
        ));
    }

    #[test]
    fn generic_bounds_and_swift_class_properties_keep_the_declared_name() {
        for (filename, line, expected) in [
            (
                "x.ts",
                "const user_file = <T extends () => void>(callback: T) => callback;",
                true,
            ),
            (
                "x.ts",
                "user_file<T extends () => void>(callback: T) {}",
                true,
            ),
            (
                "x.ts",
                "const user_file = <T extends Array<() => void>>(callback: T) => callback;",
                true,
            ),
            (
                "x.ts",
                "other<T extends () => user_file>(callback: T) {}",
                false,
            ),
            (
                "x.kt",
                "fun <T : (Int) -> Unit> user_file(callback: T) {}",
                true,
            ),
            (
                "x.kt",
                "fun <T : (Int) -> user_file> other(callback: T) {}",
                false,
            ),
            ("x.swift", "public class var user_file: Int { 1 }", true),
            (
                "x.swift",
                "public class var other: user_file { value }",
                false,
            ),
            ("x.swift", "public class func user_file() {}", true),
            ("x.swift", "public class user_file {}", true),
        ] {
            let start = line.find("user_file").unwrap() as u32;
            for fuzzy in [false, true] {
                assert_eq!(
                    is_definition_match(
                        Language::from_filename(filename) as u8,
                        line.as_bytes(),
                        &[(start, start + 9)],
                        fuzzy,
                    ),
                    expected,
                    "{filename}: {line}",
                );
            }
        }
    }

    #[test]
    fn qualified_cpp_names_require_declaration_evidence() {
        for (line, name, expected) in [
            ("    std::sort(", "sort", false),
            ("    ns::Type::method(", "method", false),
            ("    ns::Type::method(items);", "method", false),
            ("void ns::Type::method(", "method", true),
            (
                "std::vector<int> ns::Type::method() const {",
                "method",
                true,
            ),
            ("Type::Type() {", "Type", true),
            ("Type::~Type() {", "Type", true),
            ("ns::Type::Type(", "Type", true),
        ] {
            let start = line.rfind(name).unwrap() as u32;
            let end = start + name.len() as u32;
            for fuzzy in [false, true] {
                assert_eq!(
                    is_definition_match(Language::C as u8, line.as_bytes(), &[(start, end)], fuzzy),
                    expected,
                    "{line}",
                );
            }
        }
    }

    #[test]
    fn cpp_export_macros_are_skipped_before_the_type_name() {
        assert_definitions(
            "x.h",
            &[
                ("class BASE_EXPORT CommandLine {", "CommandLine", true),
                ("class BASE_EXPORT CommandLine {", "BASE_EXPORT", false),
                ("class NET_EXPORT BackoffEntry {", "BackoffEntry", true),
                (
                    "class CONTENT_EXPORT RenderWidgetHostImpl : public RenderWidgetHost,",
                    "RenderWidgetHostImpl",
                    true,
                ),
                ("struct COMPONENT_EXPORT(NETWORK_CPP) Foo {", "Foo", true),
                (
                    "struct COMPONENT_EXPORT(NETWORK_CPP) Foo {",
                    "COMPONENT_EXPORT",
                    false,
                ),
                (
                    "struct COMPONENT_EXPORT(NETWORK_CPP) Foo {",
                    "NETWORK_CPP",
                    false,
                ),
                ("class GTEST_API_ ABSL_EXPORT Test final {", "Test", true),
                ("class BASE_EXPORT CommandLine;", "CommandLine", false),
                ("struct RGB {", "RGB", true),
                ("class FOO final : public Base {", "FOO", true),
                ("struct GUID guid;", "guid", false),
                ("struct GUID guid;", "GUID", false),
                ("struct RGB make_rgb(int r) {", "make_rgb", true),
            ],
        );
    }

    #[test]
    fn typedef_bodies_continuing_on_later_lines_keep_the_tag() {
        assert_definitions(
            "x.h",
            &[
                ("typedef struct AVCodecContext {", "AVCodecContext", true),
                ("typedef enum AVMediaType {", "AVMediaType", true),
                (
                    "typedef union av_intfloat32 { uint32_t i;",
                    "av_intfloat32",
                    true,
                ),
                ("typedef struct {", "struct", false),
                (
                    "typedef struct API_AVAILABLE(macos(10.11), ios(8.0)) YADIFVTContext {",
                    "YADIFVTContext",
                    true,
                ),
                (
                    "typedef struct API_AVAILABLE(macos(10.11), ios(8.0)) YADIFVTContext {",
                    "API_AVAILABLE",
                    false,
                ),
                (
                    "typedef struct __attribute__((packed)) Header {",
                    "Header",
                    true,
                ),
                ("typedef struct Foo { int x; } Bar;", "Bar", true),
                ("typedef struct Foo { int x; } Bar;", "Foo", false),
                ("typedef struct Foo Bar;", "Bar", true),
                ("typedef struct Foo Bar;", "Foo", false),
            ],
        );
    }

    #[test]
    fn one_line_c_prototypes_match_the_multi_line_form() {
        assert_definitions(
            "x.h",
            &[
                (
                    "int sch_enc_receive(Scheduler *sch, unsigned enc_idx, AVFrame *frame);",
                    "sch_enc_receive",
                    true,
                ),
                (
                    "int av_read_frame(AVFormatContext *s, AVPacket *pkt);",
                    "av_read_frame",
                    true,
                ),
                (
                    "int av_read_frame(AVFormatContext *s,",
                    "av_read_frame",
                    true,
                ),
                ("AVFrame *av_frame_alloc(void);", "av_frame_alloc", true),
                (
                    "void av_frame_free(AVFrame **frame);",
                    "av_frame_free",
                    true,
                ),
                ("int av_foo(AVCodecContext *, int);", "av_foo", true),
                ("int av_log_get_level();", "av_log_get_level", true),
                ("void *av_malloc(size_t);", "av_malloc", true),
                ("int av_log(void *avcl, ...);", "av_log", true),
                (
                    "static int decode_init(AVCodecContext *avctx);",
                    "decode_init",
                    true,
                ),
                (
                    "  explicit CommandLine(NoProgram no_program);",
                    "CommandLine",
                    true,
                ),
                (
                    "  virtual void OnChanged(const Event& event);",
                    "OnChanged",
                    true,
                ),
                // Indented declarations without a modifier read like calls, so they stay out.
                (
                    "  void AppendSwitch(std::string_view switch_string);",
                    "AppendSwitch",
                    false,
                ),
                (
                    "  Args GetArgs(std::map<int, int>& args);",
                    "GetArgs",
                    false,
                ),
                (
                    "int av_each(int (*cb)(void *opaque), void *opaque);",
                    "av_each",
                    true,
                ),
                (
                    "  bool Wait(TimeDelta timeout = Milliseconds(1));",
                    "Wait",
                    false,
                ),
                ("  void Pause(Foo foo, int64_t ms = 5);", "Pause", false),
                ("    ret = av_read_frame(s, pkt);", "av_read_frame", false),
                ("    return av_read_frame(s, pkt);", "av_read_frame", false),
                ("    av_read_frame(s, pkt);", "av_read_frame", false),
                ("    if (av_read_frame(s, pkt) < 0)", "av_read_frame", false),
                (
                    "    int ret = av_read_frame(s, pkt);",
                    "av_read_frame",
                    false,
                ),
                ("    Foo bar(a, b);", "bar", false),
                ("    Foo bar(x * 2, \"two words\");", "bar", false),
                ("    std::unique_ptr<Foo> foo(new Foo(x));", "foo", false),
                (
                    "    std::unique_ptr<int> count(new int(0));",
                    "count",
                    false,
                ),
                (
                    "    TraceArguments args(\"foo_int\", int(10));",
                    "args",
                    false,
                ),
                ("    Foo bar(other.value(), ptr->size);", "bar", false),
                ("    Foo bar(Baz(x));", "bar", false),
                ("      MAX_COUNTER * sizeof(int64_t);", "sizeof", false),
                ("  constexpr ByteSize b(2);", "b", false),
                (
                    "  static NoDestructor<std::string> awesome(\"awesome\");",
                    "awesome",
                    false,
                ),
                ("  constexpr span<const int> span(kArray);", "span", false),
                ("  friend void Swap(Foo& a, Foo& b);", "Swap", false),
                (
                    "extern T __part_index_begin[] __attribute__((weak_import));",
                    "__attribute__",
                    false,
                ),
                ("asm volatile(\"nop\");", "volatile", false),
                ("    count * sizeof(TSFieldMapEntry)", "sizeof", false),
                (
                    "    std::string name(base::StringPrintf(\"%d items\", count));",
                    "name",
                    false,
                ),
                ("    emit valueChanged(value);", "valueChanged", false),
                ("static DEFINE_MUTEX(cache_lock);", "DEFINE_MUTEX", false),
                (
                    "static DEFINE_PER_CPU(int, cpu_count);",
                    "DEFINE_PER_CPU",
                    false,
                ),
            ],
        );
    }

    #[test]
    fn namespace_reexports_define_their_alias() {
        assert_definitions(
            "x.ts",
            &[
                ("export * as Worktree from \"./worktree\"", "Worktree", true),
                (
                    "export * as Worktree from \"./worktree\"",
                    "worktree",
                    false,
                ),
                (
                    "export * as SessionRetry from \"./retry\";",
                    "SessionRetry",
                    true,
                ),
                ("export * from \"./retry\";", "retry", false),
                (
                    "import * as Worktree from \"./worktree\";",
                    "Worktree",
                    false,
                ),
                ("  async *items() {", "items", true),
                ("  static *as() {", "as", true),
            ],
        );
    }

    #[test]
    fn zig_files_as_structs_name_their_type() {
        assert_definitions(
            "x.zig",
            &[
                ("const PageList = @This();", "PageList", true),
                ("pub const PageList = @This();", "PageList", true),
                ("const Self = @This();", "This", false),
                ("const std = @import(\"std\");", "std", true),
            ],
        );
    }

    fn assert_definitions(filename: &str, cases: &[(&str, &str, bool)]) {
        let language = Language::from_filename(filename) as u8;
        for &(line, name, expected) in cases {
            let start = line.find(name).unwrap() as u32;
            let ranges = [(start, start + name.len() as u32)];
            for fuzzy in [false, true] {
                assert_eq!(
                    is_definition_match(language, line.as_bytes(), &ranges, fuzzy),
                    expected,
                    "{filename}: {name} in {line}, fuzzy={fuzzy}",
                );
            }
        }
    }
}
