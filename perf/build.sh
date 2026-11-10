#!/usr/bin/env bash
set -euo pipefail

source_dir=$(realpath "$1")
mkdir -p "$2"
output=$(realpath "$2")
target=x86_64-unknown-linux-musl
benches=(fuzzy_search_bench grep_bench parse_bench definition_bench glob_bench bigram_bench memmem_bench)
bench_args=()
for name in "${benches[@]}"; do
  bench_args+=(--bench "$name")
done
mkdir "$output/work" "$output/bins"

docker build --tag fff-perf-build - <<'DOCKERFILE'
FROM buildpack-deps:noble@sha256:8e49144867ad1df5bce2864e890b2d783c7754efc68e2b52b27d10942edc4d0d
RUN for delay in 0 15 60 120; do \
      if [ "$delay" -gt 0 ]; then \
        echo "Retrying Ubuntu packages in $delay seconds"; \
        sleep "$delay"; \
      fi; \
      if apt-get -o Acquire::Retries=2 --error-on=any update && \
         apt-get -o Acquire::Retries=2 install -y --no-install-recommends libclang-dev; then \
        rm -rf /var/lib/apt/lists/*; \
        exit 0; \
      fi; \
    done; \
    exit 1
DOCKERFILE

docker run --rm --user "$(id -u):$(id -g)" --cap-drop ALL \
  --security-opt no-new-privileges \
  --mount "type=bind,src=$source_dir,dst=/src,readonly" \
  --mount "type=bind,src=$output/work,dst=/build" \
  --mount "type=bind,src=$(rustc --print sysroot),dst=/rust,readonly" \
  --mount "type=bind,src=$(dirname "$(readlink -f "$(command -v zig)")"),dst=/zig,readonly" \
  --mount "type=bind,src=$(command -v cargo-zigbuild),dst=/tools/cargo-zigbuild,readonly" \
  --env HOME=/build --env CARGO_TARGET_DIR=/build/target \
  --env PATH=/zig:/rust/bin:/tools:/usr/local/bin:/usr/bin:/bin \
  --workdir /src fff-perf-build \
  cargo zigbuild --locked --profile ci --target "$target" -p fff-search \
    --no-default-features --features zlob "${bench_args[@]}" --message-format=json-render-diagnostics \
  | jq -Rr 'fromjson? | if .reason == "compiler-message" then .message.rendered // empty
    elif .reason == "compiler-artifact" and .target.kind == ["bench"] and .executable
    then [.target.name, .executable] | @tsv else empty end' > "$output/build.log" || {
    cat "$output/build.log" >&2
    exit 1
  }

for name in "${benches[@]}"; do
  binary=$(awk -F '\t' -v name="$name" '$1 == name { print $2 }' "$output/build.log")
  [[ "$binary" =~ ^/build/target/$target/ci/(deps/)?${name}(-[0-9a-f]+)?$ ]]
  binary=$(realpath -e "$output/work/${binary#/build/}")
  [[ "$binary" == "$output/work/target/$target/ci/"* && -f "$binary" ]]
  headers=$(readelf -l "$binary")
  [[ "$headers" != *INTERP* ]]
  install -m 0555 "$binary" "$output/bins/$name"
done
