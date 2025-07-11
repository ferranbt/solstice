# Heavily inspired by Lighthouse: https://github.com/sigp/lighthouse/blob/stable/Makefile
# and Reth: https://github.com/paradigmxyz/reth/blob/main/Makefile
.DEFAULT_GOAL := help

FEATURES ?=

.PHONY: lint
lint: ## Run the linters
	cargo clippy -- -D warnings

.PHONY: check_format
check_format: ## Check if the code is formatted
	cargo fmt -- --check
