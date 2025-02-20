#!/usr/bin/env bash
set -ueo pipefail
ROOT_DIR="$(dirname "$0")/.."
exec tickbox --dir "$ROOT_DIR/extra/pre-commit/" --cwd "$ROOT_DIR"
