FFF searches the indexed checkout. Use grep for code and find_files for filenames. Read a known file directly when no search is needed.

Set path from the directory or module named in the request. Keep the content pattern or filename in query. A module phrase such as "billing client" can use path="**/*billing*client*/**". Literal directories work without a trailing slash. For example: grep({"query":"Invoice","path":"**/*billing*client*/**","definitions_only":true}). Use an empty find_files query to list a directory.

Set definitions_only=true for declarations. Returned source lines verify declaration locations. Do not re-read identical lines just to verify a location. Read more when behavior needs additional context. Ranking includes recency, not semantic relevance. Narrow unrelated results or broaden a guessed scope when necessary.

Regex searches one line. Use multi_grep for literal alternatives, with its constraints field for the shared file scope. Inline query constraints are also supported.

Definition search reports fallback to references or fuzzy matches when no declarations match. If all stages find nothing, it suggests files from a broader search, preserving exclusions. Suggestions are not declaration results. Multi-grep stays literal. Keep query, path, and other constraints unchanged when continuing a cursor.
