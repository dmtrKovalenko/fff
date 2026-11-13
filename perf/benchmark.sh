#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

mode=$1
state="$HOME/.local/share/fff-perf"
reference="$state/core-reference-native"
fff=5d21e4bbdf33a09f58975636208e30747bc8d918
vscode=9192ec485939d98bb8b81410e03cee0a34747573
cpus=8-11

mkdir -p "$state/ext4"
exec 9>"$state/benchmark.lock"
flock -x 9

fixture() {
  local fs=$1 name=$2 revision=$3 repo=$4
  local path="$state/$fs/$name-${revision:0:12}"
  if [[ "$mode" == prepare && ! -d "$path" ]]; then
    (
      checkout=$(mktemp -d "$state/$fs/.fixture.XXXXXX")
      trap 'rm -rf -- "$checkout"' EXIT
      git init -q "$checkout"
      git -C "$checkout" fetch -q --depth=1 "https://github.com/$repo.git" "$revision"
      git -C "$checkout" -c core.hooksPath=/dev/null checkout -q --detach "$revision"
      mv -T "$checkout" "$path"
    )
  fi
  [[ $(git -C "$path" rev-parse HEAD) == "$revision" ]]
  [[ -z $(git -C "$path" status --porcelain) ]]
}

for fs in ext4 btrfs; do
  if [[ $(findmnt -n -o FSTYPE -T "$state/$fs" | sort -u) != "$fs" ]]; then
    echo "Expected $state/$fs on $fs" >&2
    exit 1
  fi
  fixture "$fs" fff "$fff" dmtrKovalenko/fff
  fixture "$fs" vscode "$vscode" microsoft/vscode
done
[[ "$mode" != prepare ]] || exit 0

expand() {
  local part
  for part in ${1//,/ }; do seq "${part%-*}" "${part#*-}"; done
}

# perf/tune.sh sets all of this up.
tuned() {
  local cpu unit sys=/sys/devices/system/cpu
  for cpu in $(expand "$cpus"); do
    [[ $(cat "$sys/cpu$cpu/topology/thread_siblings_list") == "$cpu" ]] || return 1
    [[ ! -e "$sys/cpu$cpu/cpufreq/scaling_governor" ||
      $(cat "$sys/cpu$cpu/cpufreq/scaling_governor") == performance ]] || return 1
  done
  [[ ! -e "$sys/intel_pstate/no_turbo" || $(cat "$sys/intel_pstate/no_turbo") == 1 ]] || return 1
  [[ ! -e "$sys/cpufreq/boost" || $(cat "$sys/cpufreq/boost") == 0 ]] || return 1
  for unit in init.scope system.slice user.slice; do
    unit=$(cat "/sys/fs/cgroup/$unit/cpuset.cpus.effective") || return 1
    ! expand "$unit" | grep -qxFf <(expand "$cpus") || return 1
  done
}
if ! tuned; then
  echo "CPUs $cpus are not reserved and tuned, run perf/tune.sh as root" >&2
  exit 1
fi

output=$2
mkdir -p "$output"
work=$(mktemp -d "$state/core-run.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

stage() {
  local source=$1 dest=$2
  local bench binary
  mkdir "$dest"
  for bench in fuzzy_search grep parse definition glob bigram memmem; do
    binary="$source/${bench}_bench"
    if [[ -f "$source/targets" ]] && ! grep -Fxq "${bench}_bench" "$source/targets"; then continue; fi
    [[ -f "$binary" && ! -L "$binary" && $(stat -c %s "$binary") -le 67108864 ]]
    install -m 0555 "$binary" "$dest/${bench}_bench"
  done
}

run_bench() {
  local side=$1 bins=$2 lane=$3 bench=$4 filter=$5 fixture_path=$6
  shift 6
  [[ -f "$bins/${bench}_bench" ]] || return 0
  echo "Measuring $side $lane $bench" >&2
  if ! (
    cd "$work" || exit
    ulimit -f 8192 -v 8388608 || exit
    exec timeout --kill-after=10s 10m taskset -c "$cpus" \
      env -i PATH=/usr/bin:/bin LC_ALL=C TMPDIR="$work" CARGO_TARGET_DIR="$work/target" \
      RAYON_NUM_THREADS=4 FFF_BENCH_PATH="$fixture_path" "$@" "$bins/${bench}_bench" \
      --bench --color never --noplot --output-format bencher --nresamples 10000 \
      --warm-up-time 1 --measurement-time 2 --sample-size 30 "$filter"
  ) >"$work/stdout" 2>"$work/stderr"; then
    tail -n 20 "$work/stderr" >&2
    exit 1
  fi
  awk -v lane="$lane/$bench" '
    NF >= 8 && $1 == "test" && $(NF-5) == "..." && $(NF-4) == "bench:" &&
    $(NF-3) ~ /^[0-9,]+$/ && $(NF-2) == "ns/iter" && $(NF-1) == "(+/-" && $NF ~ /^[0-9,]+\)$/ {
      name = $2
      for (i = 3; i <= NF-6; i++) name = name "_" $i
      if (name !~ /^[-[:alnum:]_./:!*]+$/ || length(name) > 180) exit 1
      value = $(NF-3)
      gsub(",", "", value)
      if (value <= 0 || ++count > 150) exit 1
      print "test", lane "/" name, "... bench:", value, "ns/iter (+/-", $NF
      next
    }
    $1 == "test" { exit 1 }
    NF { unexpected = 1 }
    END { if (!count && unexpected) exit 1 }
  ' "$work/stdout"
}

base=
head=
first=base
sample_pair() {
  local attempt=$1
  shift
  if [[ -z "$base" ]]; then
    run_bench head "$head" "$@" >"$work/head.$attempt"
  elif [[ "$first" == base ]]; then
    run_bench base "$base" "$@" >"$work/base.$attempt"
    run_bench head "$head" "$@" >"$work/head.$attempt"
    first=head
  else
    run_bench head "$head" "$@" >"$work/head.$attempt"
    run_bench base "$base" "$@" >"$work/base.$attempt"
    first=base
  fi
}

drifted() {
  awk '
    FILENAME == ARGV[1] { base[$2] = $5; next }
    $2 in base && (10 * $5 > 11 * base[$2] || 10 * $5 < 9 * base[$2]) { found = 1 }
    END { exit !found }
  ' "$1" "$2"
}

pair() {
  local side samples=1
  local sides=(head)
  sample_pair 1 "$@"
  if [[ -n "$base" ]]; then
    sides+=(base)
  else
    awk -v prefix="test $1/$2/" 'index($0, prefix) == 1' "$output/base.bench" >"$work/base.1"
  fi
  if drifted "$work/base.1" "$work/head.1"; then
    echo "Rechecking $1 $2 after a change over 10%" >&2
    sample_pair 2 "$@"
    sample_pair 3 "$@"
    samples=3
  fi
  for side in "${sides[@]}"; do
    if (( samples == 1 )); then
      cat "$work/$side.1" >>"$output/$side.bench"
    else
      sort -s -k2,2 -k5,5n "$work/$side.1" "$work/$side.2" "$work/$side.3" |
        awk 'NR % 3 == 1 { name = $2 } $2 != name { exit 1 }
          NR % 3 == 2 { print } END { if (NR % 3) exit 1 }' >>"$output/$side.bench"
    fi
  done
}

case "$mode" in
  compare)
    stage "$3" "$work/base"
    stage "$4" "$work/head"
    base="$work/base"
    head="$work/head"
    ;;
  calibrate)
    [[ ! -e "$reference" && "$4" =~ ^[0-9a-f]{40}$ ]]
    mkdir "$work/reference"
    stage "$3" "$work/reference/bins"
    base="$work/reference/bins"
    head="$base"
    ;;
  reference)
    [[ $(sha256sum "$0" | cut -d' ' -f1) == "$(cat "$reference/script.sha256")" ]]
    (cd "$reference" && sha256sum --check --status sums.sha256)
    cp "$reference/baseline.bench" "$output/base.bench"
    head="$reference/bins"
    ;;
  *) exit 2 ;;
esac

if [[ -n "$base" ]]; then : >"$output/base.bench"; fi
: >"$output/head.bench"

for fs in ext4 btrfs; do
  for name in fff vscode; do
    if [[ "$name" == fff ]]; then
      revision=$fff
      file=crates/fff-core/src/file_picker.rs
      path_query=crates/fff-core/src
      multipart='fff-core file_picker'
    else
      revision=$vscode
      file=src/vs/editor/editor.api.ts
      path_query=src/vs/editor
      multipart='editor api'
    fi
    path="$state/$fs/$name-${revision:0:12}"
    pair "$fs/$name" fuzzy_search \
      '^fuzzy_search_repo/(empty/no_current_file|short/current_file|path/no_current_file|multipart/current_file|extension/no_current_file)$' \
      "$path" "FFF_BENCH_CURRENT_FILE=$file" \
      "FFF_BENCH_PATH_QUERY=$path_query" "FFF_BENCH_MULTIPART_QUERY=$multipart"
    pair "$fs/$name" grep '^grep_repo/' "$path"
  done
done

path="$state/ext4/fff-${fff:0:12}"
pair core parse '^realistic_queries/' "$path"
pair core definition '/mixed_256$' "$path"
pair core glob '^glob_(reuse_10k|mixed_100k)/' "$path"
pair core bigram '^bigram_query_100000/(medium_6char|long_14char|multi_word)$|^bigram_build/short_content/10000$' "$path"
pair core memmem '/file_picker\.rs/53KB/(medium|long)/miss$' "$path"

[[ -s "$output/base.bench" || -s "$output/head.bench" ]]
if [[ "$mode" != compare ]]; then
  diff -u <(awk '{print $2}' "$output/base.bench" | sort) \
    <(awk '{print $2}' "$output/head.bench" | sort)
fi
if [[ "$mode" == calibrate ]]; then
  cp "$output/base.bench" "$work/reference/baseline.bench"
  sha256sum "$0" | cut -d' ' -f1 >"$work/reference/script.sha256"
  printf '%s\n' "$4" >"$work/reference/revision"
  (cd "$work/reference" && sha256sum bins/* baseline.bench >sums.sha256)
  mv -T "$work/reference" "$reference"
fi
