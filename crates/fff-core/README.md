# fff

fff is a file search toolkit. It is faster than ripgrep and fzf and designed for a long running applications like file editors, ai agents, or file exploerers.

> [!Important performance information]  
> For the most optimized fff build use `zlob` feature. It requires zig v0.17.1 or newer to be installed on the machine.

## Features

- Fuzzy file name search 
- Typo resistance
- Frecency and query history ranking
- Native git support via libgit
- Advanced ranking 
- Grep functionality with SIMD optimized plain matcher and regex
- Multi grep using aho-corasick algorithm
- Efficient memory mapping for file system
- Cross platform support (Linux, Windows, MacOS)
- Advnaced constraints syntax allowing to prefilter based on git status, glob, extension, size, timing and more

## Performance

FFF is designed for high performance and low latency. SIMD optimized where needed, parallelized for multi core systems, efficient sorting and ranking algorithms, memaps and much more.

On MacOS FFF is about 20-50 times faster than ripgrep for content search and around 10 times faster than fzf for file name search.

## Documentation

Refer rust docs https://docs.rs/crate/fff-search/latest
