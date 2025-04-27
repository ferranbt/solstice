# Heavily inspired by Lighthouse: https://github.com/sigp/lighthouse/blob/stable/Makefile
# and Reth: https://github.com/paradigmxyz/reth/blob/main/Makefile
.DEFAULT_GOAL := help

FEATURES ?=

.PHONY: lint
lint: ## Run the linters
	cargo fmt -- --check
	cargo clippy --features "$(FEATURES)" -- -D warnings
