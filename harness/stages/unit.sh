#!/usr/bin/env bash
set -euo pipefail
SEED=0; while [[ $# -gt 0 ]]; do case "$1" in --seed) SEED="$2"; shift 2;; *) shift;; esac; done
echo '{"tests_run":0,"tests_passed":0}'

