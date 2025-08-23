#!/usr/bin/env bash
set -euo pipefail
SEED=0; while [[ $# -gt 0 ]]; do case "$1" in --seed) SEED="$2"; shift 2;; *) shift;; esac; done
echo '{"cases_run":0,"cases_passed":0}'

