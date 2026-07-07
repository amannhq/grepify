# Cross-language parity harness for Grepify.
#
#   make setup    # install the TS driver's tsx (needs @grepify/node built)
#   make parity   # run the full parity harness (Python vs TypeScript)
#   make keep     # same, but keep the .work scratch dir (db/out) for inspection
#   make clean    # remove the .work scratch dir
#
# Prerequisites:
#   - Python side: `uv run maturin develop` at the repo root (builds grepify).
#   - TS side: the @grepify/node native artifact in packages/node
#     (`cd packages/node && npm install && npm run build`) plus `make setup` here.

HERE := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

.PHONY: parity keep setup clean

setup:
	cd ts && npm install

parity:
	python3 run_parity.py

keep:
	python3 run_parity.py --keep

clean:
	rm -rf .work
