#!/usr/bin/env bash
set -euo pipefail

base_ref="${1:-}"

if [[ -z "$base_ref" ]]; then
  if [[ -n "${GITHUB_BASE_REF:-}" ]]; then
    base_ref="origin/${GITHUB_BASE_REF}"
  elif [[ -n "${GITHUB_EVENT_BEFORE:-}" && ! "${GITHUB_EVENT_BEFORE}" =~ ^0+$ ]]; then
    base_ref="${GITHUB_EVENT_BEFORE}"
  elif git describe --tags --abbrev=0 HEAD^ >/dev/null 2>&1; then
    base_ref="$(git describe --tags --abbrev=0 HEAD^)"
  elif git rev-parse HEAD^ >/dev/null 2>&1; then
    base_ref="HEAD^"
  else
    base_ref="$(git rev-list --max-parents=0 HEAD)"
  fi
fi

if ! git rev-parse --verify "$base_ref^{commit}" >/dev/null 2>&1; then
  echo "cannot resolve rustfmt base ref: $base_ref" >&2
  exit 2
fi

mapfile -t rust_files < <(
  git diff --name-only --diff-filter=ACMR "$base_ref"...HEAD -- '*.rs' \
    | while IFS= read -r file; do
        [[ -f "$file" ]] && printf '%s\n' "$file"
      done
)

if [[ ${#rust_files[@]} -eq 0 ]]; then
  echo "no changed Rust files"
  exit 0
fi

printf 'rustfmt checking %d changed Rust files against %s\n' "${#rust_files[@]}" "$base_ref"
rustfmt --edition 2021 --check "${rust_files[@]}"
