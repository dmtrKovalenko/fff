#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

# Prints the markdown report, exits 1 when a benchmark slowed down by over 10%.
awk '
  function human(ns) {
    if (ns >= 1e9) return sprintf("%.2f s", ns / 1e9)
    if (ns >= 1e6) return sprintf("%.2f ms", ns / 1e6)
    if (ns >= 1e3) return sprintf("%.2f µs", ns / 1e3)
    return sprintf("%d ns", ns)
  }
  function row(i) {
    return sprintf("| %s | `%s` | %s | %s | %+.2f%% |\n", mark[i], name[i], human(before[i]), human(after[i]), ratio[i] - 100)
  }
  function series(kind, i, out, value) {
    for (i = 1; i <= n; i++) {
      value = kind == mark[i] ? ratio[i] : 0
      if (kind ~ /^[0-9]+$/) value = kind
      out = out (i > 1 ? ", " : "") sprintf("%.1f", value)
    }
    return "[" out "]"
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
    if (ratio[n] > max) max = ratio[n]
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

    high = 10 * (int((max > 120 ? max : 120) / 10) + 1)
    print "```mermaid"
    print "---"
    print "config:"
    printf "  xyChart: { width: 900, height: %d }\n", 120 + 22 * n
    print "  themeVariables: { xyChart: { plotColorPalette: \"#d1242f, #1a7f37, #8b949e, #6e7781, #d1242f\" } }"
    print "---"
    print "xychart-beta horizontal"
    print "  title \"PR time as % of base, red line is the 110% limit\""
    printf "  x-axis ["
    for (i = 1; i <= n; i++) {
      parts = split(name[i], part, "/")
      skip = part[1] == "core" ? 2 : 3
      label = ""
      for (p = 1; p <= parts; p++) if (p != skip) label = label (label == "" ? "" : "/") part[p]
      printf "%s\"%s\"", (i > 1 ? ", " : ""), label
    }
    print "]"
    printf "  y-axis \"%% of base\" 0 --> %d\n", high
    print "  bar " series("🔴")
    print "  bar " series("🟢")
    print "  bar " series("⚪")
    print "  line " series("100")
    print "  line " series("110")
    print "```"

    printf "\n<details><summary>🔬 All %d Criterion timings</summary>\n\n%s", n, header
    for (i = 1; i <= n; i++) printf "%s", row(i)
    print "\n</details>"
    exit slower > 0
  }
' "$1" "$2"
