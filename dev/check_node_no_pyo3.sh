#!/usr/bin/env bash
# Fails if the Grepify Node bindings pull PyO3 into their dependency tree.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if cargo tree -p grepify_node -e normal | grep -q pyo3; then
  echo "ERROR: grepify_node depends on pyo3 (must stay PyO3-free)" >&2
  cargo tree -p grepify_node -e normal | grep pyo3 >&2 || true
  exit 1
fi

echo "grepify_node dependency tree is PyO3-free"
