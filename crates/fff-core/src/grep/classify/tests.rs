use super::DefinitionKind::*;
use super::*;

/// `(lang, line, expected (kind, name))`; `None` = not a definition.
type Case = (Lang, &'static str, Option<(DefinitionKind, &'static str)>);

fn check(cases: &[Case]) {
    let mut failures = Vec::new();
    for &(lang, line, expected) in cases {
        let got = classify_line(line, lang)
            .map(|d| (d.kind, &line[d.name.0 as usize..d.name.1 as usize]));
        if got != expected {
            failures.push(format!(
                "{lang:?} {line:?}: expected {expected:?}, got {got:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn rust() {
    use Lang::Rust as L;
    check(&[
        (L, "fn main() {}", Some((Function, "main"))),
        (
            L,
            "#[test] fn main_test() {}",
            Some((Function, "main_test")),
        ),
        (
            L,
            "    pub(crate) async unsafe fn run<T>(x: T) -> Foo {",
            Some((Function, "run")),
        ),
        (
            L,
            "pub(in crate::grep) fn helper() {",
            Some((Function, "helper")),
        ),
        (L, "pub const fn new() -> Self {", Some((Function, "new"))),
        (
            L,
            "pub extern \"C\" fn fff_free(p: *mut u8) {",
            Some((Function, "fff_free")),
        ),
        (L, "pub struct GrepMatch {", Some((Type, "GrepMatch"))),
        (L, "pub enum Lang {", Some((Type, "Lang"))),
        (L, "pub unsafe trait Send {}", Some((Type, "Send"))),
        (
            L,
            "pub type Result<T> = std::result::Result<T, Error>;",
            Some((Type, "Result")),
        ),
        (L, "mod tests {", Some((Module, "tests"))),
        (
            L,
            "pub const MAX_LEN: usize = 512;",
            Some((Constant, "MAX_LEN")),
        ),
        (
            L,
            "static mut COUNTER: u32 = 0;",
            Some((Constant, "COUNTER")),
        ),
        (L, "fn r#type() {}", Some((Function, "r#type"))),
        (
            L,
            "impl<T: Fn() -> u8> Display for Wrapper<T> where T: Copy {",
            Some((Impl, "Display for Wrapper<T>")),
        ),
        (L, "impl Parser<'_> {", Some((Impl, "Parser<'_>"))),
        (L, "macro_rules! words {", Some((Macro, "words"))),
        (L, "    let x = render(frame);", None),
        (L, "    render(frame)", None),
        (L, "pub use crate::grep::GrepMatch;", None),
        (L, "unsafe { foo() }", None),
        (L, "// fn commented() {}", None),
        (L, "extern crate core;", None),
        (L, "", None),
        (L, "      ", None),
    ]);
}

#[test]
fn go() {
    use Lang::Go as L;
    check(&[
        (L, "func main() {", Some((Function, "main"))),
        (
            L,
            "func (s *Server) Handle(w http.ResponseWriter) error {",
            Some((Function, "Handle")),
        ),
        (L, "func Map[T any](xs []T) []T {", Some((Function, "Map"))),
        (L, "type Server struct {", Some((Type, "Server"))),
        (
            L,
            "var ErrNotFound = errors.New(\"x\")",
            Some((Variable, "ErrNotFound")),
        ),
        (L, "const MaxSize = 10", Some((Constant, "MaxSize"))),
        (L, "\tvar x = 5", None),
        (L, "\tdefer func() {", None),
        (L, "\tgo func() {", None),
        (L, "type (", None),
    ]);
}

#[test]
fn python() {
    use Lang::Python as L;
    check(&[
        (L, "def main():", Some((Function, "main"))),
        (
            L,
            "    async def fetch(self, url):",
            Some((Function, "fetch")),
        ),
        (L, "class Foo(Base):", Some((Type, "Foo"))),
        (L, "@dataclass class Point:", Some((Type, "Point"))),
        (L, "    return define(x)", None),
        (L, "    x = Foo()", None),
    ]);
}

#[test]
fn javascript() {
    use Lang::JavaScript as L;
    check(&[
        (L, "function render(frame) {", Some((Function, "render"))),
        (
            L,
            "export async function* stream() {",
            Some((Function, "stream")),
        ),
        (
            L,
            "export default class App extends Base {",
            Some((Type, "App")),
        ),
        (L, "export interface Props {", Some((Type, "Props"))),
        (L, "export type Id = string;", Some((Type, "Id"))),
        (L, "export const enum Mode {", Some((Type, "Mode"))),
        (L, "declare module 'foo' {", None),
        (
            L,
            "export const Button = () => null",
            Some((Function, "Button")),
        ),
        (
            L,
            "const handler = async (req) => {",
            Some((Function, "handler")),
        ),
        (L, "const LIMIT = 10;", Some((Constant, "LIMIT"))),
        (
            L,
            "export let state: State = init();",
            Some((Variable, "state")),
        ),
        (
            L,
            "  constructor(private readonly db: Db) {",
            Some((Function, "constructor")),
        ),
        (
            L,
            "  async load(id: string): Promise<User> {",
            Some((Function, "load")),
        ),
        (L, "  static create<T>(x: T) {", Some((Function, "create"))),
        (L, "  #secret() {", Some((Function, "secret"))),
        (L, "  get value() {", Some((Function, "value"))),
        (L, "  const x = 5;", None),
        (L, "  if (x) {", None),
        (L, "  } catch (e) {", None),
        (L, "  describe('works', () => {", None),
        (L, "  useEffect(() => {", None),
        (L, "  foo(x).then(y)", None),
        (L, "  items.map((x) => {", None),
        (L, "module.exports = foo", None),
        (L, "export { foo, bar }", None),
        (L, "  return function () {", None),
    ]);
}

#[test]
fn lua() {
    use Lang::Lua as L;
    check(&[
        (
            L,
            "function M.render(item, ctx)",
            Some((Function, "M.render")),
        ),
        (
            L,
            "local function build_line(width)",
            Some((Function, "build_line")),
        ),
        (
            L,
            "function Picker:open(opts)",
            Some((Function, "Picker:open")),
        ),
        (
            L,
            "M.search = function(query)",
            Some((Function, "M.search")),
        ),
        (L, "local cb = function()", Some((Function, "cb"))),
        (L, "  local x = render(item)", None),
        (L, "  if a == function_name then", None),
        (L, "  pcall(function()", None),
    ]);
}

#[test]
fn c_family() {
    use Lang::C as L;
    check(&[
        (
            L,
            "int main(int argc, char **argv) {",
            Some((Function, "main")),
        ),
        (
            L,
            "static inline const char *name_of(struct item *it)",
            Some((Function, "name_of")),
        ),
        (
            L,
            "struct foo *make_foo(void) {",
            Some((Function, "make_foo")),
        ),
        (
            L,
            "std::vector<int> Parser::parse(const std::string& s) const {",
            Some((Function, "parse")),
        ),
        (L, "Foo::~Foo() {", Some((Function, "Foo"))),
        (
            L,
            "template <typename T> T max_of(T a, T b) {",
            Some((Function, "max_of")),
        ),
        (
            L,
            "[[nodiscard]] int compute(int x) {",
            Some((Function, "compute")),
        ),
        (
            L,
            "static int parse_args(int argc,",
            Some((Function, "parse_args")),
        ),
        (L, "struct point {", Some((Type, "point"))),
        (
            L,
            "class Widget final : public Base {",
            Some((Type, "Widget")),
        ),
        (L, "enum class Color : uint8_t {", Some((Type, "Color"))),
        (L, "namespace fff {", Some((Module, "fff"))),
        (L, "typedef struct point point_t;", Some((Type, "point_t"))),
        (
            L,
            "typedef void (*callback)(int);",
            Some((Type, "callback")),
        ),
        (L, "typedef int vec3[3];", Some((Type, "vec3"))),
        (L, "#define MAX_LEN 512", Some((Macro, "MAX_LEN"))),
        (L, "# define ALIGN(x) x", Some((Macro, "ALIGN"))),
        (L, "int compute(int x);", None),
        (L, "struct point;", None),
        (L, "struct point p = {0};", None),
        (L, "    foo(x);", None),
        (L, "    int x = foo(y);", None),
        (L, "    return compute(x);", None),
        (L, "    if (x) {", None),
        (L, "    } else if (y) {", None),
        (L, "    list_for_each(pos, head) {", None),
        (L, "    printf(\"%d\", x);", None),
        (L, "#include <stdio.h>", None),
        (L, "void (*cb)(int);", None),
    ]);
}

#[test]
fn java_family() {
    use Lang::Java as L;
    check(&[
        (L, "public class UserService {", Some((Type, "UserService"))),
        (
            L,
            "    public static void main(String[] args) {",
            Some((Function, "main")),
        ),
        (
            L,
            "    @Override public String toString() {",
            Some((Function, "toString")),
        ),
        (
            L,
            "    public static <T> List<T> of(T... xs) {",
            Some((Function, "of")),
        ),
        (
            L,
            "    public UserService(Repo repo) {",
            Some((Function, "UserService")),
        ),
        (
            L,
            "    private final Map<String, List<User>> byName() {",
            Some((Function, "byName")),
        ),
        (
            L,
            "public record Point(int x, int y) {",
            Some((Type, "Point")),
        ),
        (
            L,
            "    [HttpGet] public async Task<IActionResult> Get(int id) {",
            Some((Function, "Get")),
        ),
        (
            L,
            "    public int Count() => items.Count;",
            Some((Function, "Count")),
        ),
        (L, "namespace App.Services {", Some((Module, "App"))),
        (
            L,
            "  Widget build(BuildContext context) {",
            Some((Function, "build")),
        ),
        (L, "        return new Foo(x) {", None),
        (L, "        Foo foo = new Foo(x);", None),
        (L, "        System.out.println(x);", None),
        (L, "        list.forEach(x -> {", None),
        (L, "        super(x);", None),
        (L, "    void abstractThing();", None),
        (L, "        synchronized (lock) {", None),
    ]);
}

#[test]
fn kotlin_scala_swift() {
    check(&[
        (Lang::Kotlin, "fun main() {", Some((Function, "main"))),
        (
            Lang::Kotlin,
            "    suspend fun <T> List<T>.chunked(n: Int): List<T> {",
            Some((Function, "List<T>.chunked")),
        ),
        (
            Lang::Kotlin,
            "    suspend fun <T> Foo.bar(n: Int): T {",
            Some((Function, "Foo.bar")),
        ),
        (
            Lang::Kotlin,
            "data class User(val name: String)",
            Some((Type, "User")),
        ),
        (Lang::Kotlin, "enum class Color {", Some((Type, "Color"))),
        (
            Lang::Kotlin,
            "@Composable fun Screen() {",
            Some((Function, "Screen")),
        ),
        (
            Lang::Kotlin,
            "val DEFAULT_TIMEOUT = 30",
            Some((Constant, "DEFAULT_TIMEOUT")),
        ),
        (Lang::Kotlin, "    val x = 5", None),
        (
            Lang::Scala,
            "case class Point(x: Int, y: Int)",
            Some((Type, "Point")),
        ),
        (
            Lang::Scala,
            "  override def toString: String =",
            Some((Function, "toString")),
        ),
        (
            Lang::Scala,
            "object Main extends App {",
            Some((Type, "Main")),
        ),
        (
            Lang::Swift,
            "    @objc public func tap(_ sender: Any) {",
            Some((Function, "tap")),
        ),
        (
            Lang::Swift,
            "struct ContentView: View {",
            Some((Type, "ContentView")),
        ),
        (Lang::Swift, "extension String {", Some((Impl, "String"))),
        (
            Lang::Swift,
            "    class func make() -> Self {",
            Some((Function, "make")),
        ),
        (Lang::Swift, "protocol Drawable {", Some((Type, "Drawable"))),
    ]);
}

#[test]
fn ruby_elixir_php() {
    check(&[
        (Lang::Ruby, "  def valid?", Some((Function, "valid?"))),
        (
            Lang::Ruby,
            "  def self.build(attrs)",
            Some((Function, "self.build")),
        ),
        (
            Lang::Ruby,
            "class Admin::User < ApplicationRecord",
            Some((Type, "Admin::User")),
        ),
        (Lang::Ruby, "module Helpers", Some((Module, "Helpers"))),
        (Lang::Ruby, "class << self", None),
        (
            Lang::Elixir,
            "  defp do_parse(input, acc) do",
            Some((Function, "do_parse")),
        ),
        (
            Lang::Elixir,
            "defmodule MyApp.Router do",
            Some((Module, "MyApp.Router")),
        ),
        (
            Lang::Elixir,
            "  def valid?(x), do: x",
            Some((Function, "valid?")),
        ),
        (
            Lang::Elixir,
            "  defstruct [:name, :age]",
            Some((Type, "defstruct")),
        ),
        (
            Lang::Php,
            "    public static function create(array $data): self",
            Some((Function, "create")),
        ),
        (
            Lang::Php,
            "final class UserController extends Controller",
            Some((Type, "UserController")),
        ),
        (
            Lang::Php,
            "    #[Route('/x')] public function index()",
            Some((Function, "index")),
        ),
    ]);
}

#[test]
fn zig_shell_ml_haskell() {
    check(&[
        (Lang::Zig, "pub fn main() !void {", Some((Function, "main"))),
        (
            Lang::Zig,
            "pub const Parser = struct {",
            Some((Type, "Parser")),
        ),
        (Lang::Zig, "const Color = enum(u8) {", Some((Type, "Color"))),
        (
            Lang::Zig,
            "const max_len = 512;",
            Some((Constant, "max_len")),
        ),
        (Lang::Zig, "    const x = foo();", None),
        (
            Lang::Zig,
            "    pub const Inner = struct {",
            Some((Type, "Inner")),
        ),
        (
            Lang::Shell,
            "build_release() {",
            Some((Function, "build_release")),
        ),
        (
            Lang::Shell,
            "function cleanup {",
            Some((Function, "cleanup")),
        ),
        (Lang::Shell, "git-sync () {", Some((Function, "git-sync"))),
        (Lang::Shell, "  echo \"hi\"", None),
        (Lang::Ml, "let rec parse input =", Some((Function, "parse"))),
        (
            Lang::Ml,
            "let make = (~title) => {",
            Some((Function, "make")),
        ),
        (Lang::Ml, "type t = { name: string }", Some((Type, "t"))),
        (Lang::Ml, "module type S = sig", Some((Type, "S"))),
        (Lang::Ml, "  let x = 5 in", None),
        (Lang::Haskell, "main :: IO ()", Some((Function, "main"))),
        (
            Lang::Haskell,
            "data Shape = Circle Float | Square Float",
            Some((Type, "Shape")),
        ),
        (
            Lang::Haskell,
            "instance Show Shape where",
            Some((Impl, "Show")),
        ),
        (Lang::Haskell, "  where go = 1", None),
    ]);
}

#[test]
fn unknown_language_uses_common_keywords() {
    check(&[
        (
            Lang::Unknown,
            "pub fn helper() {}",
            Some((Function, "helper")),
        ),
        (Lang::Unknown, "class Foo:", Some((Type, "Foo"))),
        (Lang::Unknown, "def bar", Some((Function, "bar"))),
        (Lang::Unknown, "call(x)", None),
    ]);
    assert!(is_definition_line("  export default function main() {"));
    assert!(!is_definition_line("  main();"));
}

#[test]
fn languages_from_file_names() {
    assert_eq!(Lang::from_file_name("main.rs"), Lang::Rust);
    assert_eq!(Lang::from_file_name("App.TSX"), Lang::JavaScript);
    assert_eq!(Lang::from_file_name("picker.lua"), Lang::Lua);
    assert_eq!(Lang::from_file_name("vec.hpp"), Lang::C);
    assert_eq!(Lang::from_file_name("Main.cs"), Lang::Java);
    assert_eq!(Lang::from_file_name("Rakefile"), Lang::Ruby);
    assert_eq!(Lang::from_file_name("Component.res"), Lang::Ml);
    assert_eq!(Lang::from_file_name("README"), Lang::Unknown);
    assert_eq!(Lang::from_file_name("CHANGELOG.md"), Lang::Text);
    assert!(classify_line("class Handler is described here", Lang::Text).is_none());
    assert_eq!(Lang::from_file_name("archive.verylongext"), Lang::Unknown);
}

#[test]
fn only_matches_on_the_header_are_definitions() {
    let line = "pub fn render(frame: Frame) -> Svgr {";
    let def = classify_line(line, Lang::Rust).unwrap();
    let range = |needle: &str| {
        let s = line.find(needle).unwrap() as u32;
        (s, s + needle.len() as u32)
    };
    assert!(def.is_hit_by(&[range("render")]));
    assert!(def.is_hit_by(&[range("pub fn")]));
    assert!(def.is_hit_by(&[range("Frame"), range("render")]));
    assert!(!def.is_hit_by(&[range("Frame")]));
    assert!(!def.is_hit_by(&[range("Svgr")]));
    assert!(!def.is_hit_by(&[]));
}

#[test]
fn never_panics_on_odd_input() {
    let langs = [
        Lang::Rust,
        Lang::Go,
        Lang::Python,
        Lang::JavaScript,
        Lang::Lua,
        Lang::C,
        Lang::Java,
        Lang::Kotlin,
        Lang::Scala,
        Lang::Swift,
        Lang::Ruby,
        Lang::Elixir,
        Lang::Zig,
        Lang::Php,
        Lang::Shell,
        Lang::Ml,
        Lang::Haskell,
        Lang::Text,
        Lang::Unknown,
    ];
    let lines = [
        "fn",
        "fn ",
        "pub(",
        "impl<",
        "impl",
        "macro_rules!",
        "#[",
        "@",
        "@foo(",
        "#",
        "#define",
        "typedef",
        "typedef ;",
        "func (",
        "struct",
        "const",
        "extern \"",
        "x :: ",
        "a = function",
        "f(",
        "a b(",
        "template <",
        "[[",
        "function*",
        "let",
        "def ",
        "()",
        "ü fn ü(",
        "\t\t",
        "fn \u{1F600}()",
        "class Ä {",
    ];
    for lang in langs {
        for line in lines {
            let _ = classify_line(line, lang);
        }
    }
}
