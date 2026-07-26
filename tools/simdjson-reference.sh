#!/usr/bin/env bash
set -euo pipefail

readonly commit=8e6bac94877f2d3d026000d36ce81e0aaf38d26f
readonly root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly reference_dir="${root_dir}/target/simdjson-reference"
readonly build_dir="${reference_dir}/build-reference"
readonly iterations="${SIMDLEX_REFERENCE_ITERATIONS:-200}"

if [[ ! -d "${reference_dir}/.git" ]]; then
  git clone --filter=blob:none --no-checkout \
    https://github.com/simdjson/simdjson.git "${reference_dir}"
fi

git -C "${reference_dir}" fetch origin "${commit}"
git -C "${reference_dir}" checkout --detach "${commit}"

cmake -S "${reference_dir}" -B "${build_dir}" \
  -DCMAKE_BUILD_TYPE=Release \
  -DSIMDJSON_DEVELOPER_MODE=ON
cmake --build "${build_dir}" --target parse --parallel

c++ -std=c++17 -O3 -DNDEBUG \
  -I"${reference_dir}/singleheader" \
  "${reference_dir}/singleheader/simdjson.cpp" \
  "${root_dir}/tools/simdjson_ondemand.cpp" \
  -o "${build_dir}/simdjson_ondemand"

readonly twitter="${reference_dir}/jsonexamples/twitter.json"
readonly citm="${reference_dir}/jsonexamples/citm_catalog.json"
readonly parse="${build_dir}/benchmark/dom/parse"

for implementation in haswell westmere; do
  "${parse}" -H -s stage1 -a "${implementation}" -n "${iterations}" \
    "${twitter}" "${citm}"
  "${build_dir}/simdjson_ondemand" "${implementation}" "${iterations}" \
    "${twitter}" "${citm}"
done

cargo run --manifest-path "${root_dir}/Cargo.toml" --release \
  --features json-test-grammar --example reference_bench -- \
  "${iterations}" "${twitter}" "${citm}"
