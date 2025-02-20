#!/usr/bin/env bash
set -ueo pipefail
exec cargo fmt -- --check
