FFF searches this checkout. Use grep for code and find_files for filenames. Read a known path directly when no search is needed.

Keep module context in a path constraint, not in fuzzy search words. For a module phrase such as "billing client", use "**/*billing*client*/**". For a known directory, use "src/net/". For example: grep({"query":"**/*billing*client*/** Invoice","definitions_only":true}) or find_files({"query":"**/*billing*client*/** retry"}). A directory glob prevents unrelated fuzzy filename matches.

Put grep constraints before the content pattern. Constraints can be directories ending in /, filenames, globs, or exclusions prefixed with !. Bare words are content. Use multi_grep for literal alternatives, with the path filter in constraints. Regex searches one line.

Set definitions_only=true for declarations. Returned source lines verify declaration locations. Do not re-read identical lines just to verify a location. Read more when behavior needs additional context. Ranking includes recency, not semantic relevance. Narrow unrelated results or broaden a guessed scope when necessary.

Definition search reports fallback to references or fuzzy matches when no declarations match. Multi-grep stays literal. Keep the query and constraints unchanged when continuing a cursor.
