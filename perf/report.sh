#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

# Prints the markdown report, exits 1 when a benchmark slowed down by over 10%.
awk -v chart="${3:-}" '
  function human(ns) {
    if (ns >= 1e9) return sprintf("%.2f s", ns / 1e9)
    if (ns >= 1e6) return sprintf("%.2f ms", ns / 1e6)
    if (ns >= 1e3) return sprintf("%.2f µs", ns / 1e3)
    return sprintf("%d ns", ns)
  }
  function row(i) {
    return sprintf("| %s | `%s` | %s | %s | %+.2f%% |\n", mark[i], name[i], human(before[i]), human(after[i]), ratio[i] - 100)
  }
  NR == FNR { base[$2] = $5; next }
  $2 in base {
    n++
    name[n] = $2
    before[n] = base[$2]
    after[n] = $5
    ratio[n] = 100 * $5 / base[$2]
    mark[n] = "⚪"
    if (10 * $5 > 11 * base[$2]) { mark[n] = "🔴"; slower++ }
    if (10 * $5 < 9 * base[$2]) { mark[n] = "🟢"; faster++ }
  }
  END {
    if (!n) exit 2
    header = "| | Benchmark | Base | PR | Change |\n| --- | --- | ---: | ---: | ---: |\n"
    printf "🔴 %d slower · 🟢 %d faster · ⚪ %d within ±10%%\n\n", slower, faster, n - slower - faster
    if (slower + faster) {
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
