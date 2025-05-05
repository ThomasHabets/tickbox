#!/usr/bin/env bash
exec cargo clippy --color=always --all-targets -- -D warnings
