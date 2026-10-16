FFF searches the indexed checkout. Use grep for code and find_files for filenames. Read a known file directly when needed.

For content questions, call grep with the module scope. The path field accepts a complete directory name anywhere in the checkout. Use a glob such as **/*transport*/** for a partial module name. Combine related content patterns with | in regex mode. Small result sets include surrounding source automatically. Set context when you need a specific number of lines.

Results contain source text with one-based line numbers. These lines verify declaration locations. Read additional source only when required evidence is missing. Set definitions_only=true when the query matches a declaration name. Search behavior, comments, and fields without that filter.

Use paths for exact checkout-relative files or directories. Use globs for rg -g patterns. Regex and literal modes preserve the query and default to case-sensitive matching. Set case="insensitive" for rg -i. The default page contains up to 100 matches. Keep search arguments unchanged when continuing a cursor.

find_files accepts path fragments, glob mode, or regex mode. Path fragments treat hyphens and underscores alike. Fallback suggestions are labeled and may lie outside the requested scope. Exclusions still apply. Files absent from the index remain unavailable.
