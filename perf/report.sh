#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

# Prints the markdown report, exits 1 when a benchmark slowed down by over 10%.
awk -v chart="${3:-}" '
  function human(ns) {
    if (!ns) return "—"
    if (ns >= 1e9) return sprintf("%.2f s", ns / 1e9)
    if (ns >= 1e6) return sprintf("%.2f ms", ns / 1e6)
    if (ns >= 1e3) return sprintf("%.2f µs", ns / 1e3)
    return sprintf("%d ns", ns)
  }
  function row(i, bench, fence) {
    bench = name[i]
    gsub(/\|/, "\\\\|", bench)
    fence = "`"
    while (index(bench, fence)) fence = fence "`"
    if (length(fence) > 1) bench = " " bench " "
    return sprintf("| %s | %s%s%s | %s | %s | %s |\n", mark[i], fence, bench, fence, human(base[name[i]]), human(head[name[i]]), change[i])
  }
  $1 != "test" || $3 != "..." || $4 != "bench:" || $5 !~ /^[0-9]+$/ || $5 <= 0 || $6 != "ns/iter" { next }
  !seen[$2]++ { name[++n] = $2 }
  FILENAME == ARGV[1] { base[$2] = $5; next }
  { head[$2] = $5 }
  END {
    if (!n) exit 2
    for (i = 1; i <= n; i++) {
      b = base[name[i]]; h = head[name[i]]
      if (!b) { mark[i] = "🔵"; change[i] = "New"; added++; continue }
      if (!h) { mark[i] = "➖"; change[i] = "Removed"; removed++; continue }
      change[i] = sprintf("%+.2f%%", 100 * (h - b) / b)
      if (10*h > 11*b) { mark[i] = "🔴"; slower++ }
      else if (10*h < 9*b) { mark[i] = "🟢"; faster++ }
      else { mark[i] = "⚪"; within++ }
    }
    header = "| | Benchmark | Base | PR | Change |\n| --- | --- | ---: | ---: | ---: |\n"
    printf "🔴 %d slower, 🟢 %d faster, ⚪ %d within ±10%%", slower, faster, within
    if (added || removed) printf ", %d new, %d removed", added, removed
    print "\n"
    if (slower + faster + added + removed) {
      printf "%s", header
      for (i = 1; i <= n; i++) if (mark[i] != "⚪") printf "%s", row(i)
      print ""
    }

    if (chart != "") printf "![PR time against base for every benchmark](%s)\n", chart
    printf "\n<details><summary>🔬 All %d Criterion timings</summary>\n\n%s", n, header
    for (i = 1; i <= n; i++) printf "%s", row(i)
    print "\n</details>"
    exit slower > 0
  }
' "$1" "$2"
