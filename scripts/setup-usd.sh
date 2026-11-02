#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
"${PYTHON:-python3}" -m venv .usd-runtime
.usd-runtime/bin/python -m pip install 'usd-core==26.11'
.usd-runtime/bin/python -c 'from pxr import Usd; print("OpenUSD", Usd.GetVersion())'
