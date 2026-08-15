SHELL := /bin/bash

.PHONY: help setup build test clean format lint check install run-local deploy docs \
        coverage-solidity pre-push pre-push-audit audit-solidity-test audit-an-test \
        setup-an-audit-tools setup-audit-vendors pre-push-an production-preflight relayer-test relayer-fmt relayer-clippy \
        bootstrap-foundry-deps bootstrap-an-audit-py sync-audit-deposit-fixtures

# Default target
.DEFAULT_GOAL := help

# Ensure Foundry is in PATH
export PATH := $(HOME)/.foundry/bin:$(PATH)

# Colors
BLUE := \033[0;34m
GREEN := \033[0;32m
YELLOW := \033[1;33m
NC := \033[0m # No Color

help: ## Show this help message
	@echo "$(BLUE)Acki Nacki Bridge - Available Commands$(NC)"
	@echo ""
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "  $(GREEN)%-20s$(NC) %s\n", $$1, $$2}'
	@echo ""

setup: ## Run initial setup (install dependencies and tools)
	@echo "$(BLUE)Running setup...$(NC)"
	@chmod +x setup.sh
	@./setup.sh

build: ## Build all components (Rust + Solidity)
	@echo "$(BLUE)Building project...$(NC)"
	@chmod +x build.sh
	@./build.sh

build-release: ## Build in release mode
	@echo "$(BLUE)Building project (release mode)...$(NC)"
	@chmod +x build.sh
	@./build.sh --release

build-rust: ## Build only Rust workspace
	@echo "$(BLUE)Building Rust workspace...$(NC)"
	@cargo build --workspace

bootstrap-foundry-deps: ## Install gitignored forge-std + npm deps (post-clone, Linux/macOS)
	@chmod +x scripts/bootstrap-foundry-deps.sh scripts/bootstrap-cargo-git.sh
	@source ./scripts/bootstrap-cargo-git.sh && ./scripts/bootstrap-foundry-deps.sh

build-solidity: bootstrap-foundry-deps ## Build only Solidity contracts
	@echo "$(BLUE)Building Solidity contracts...$(NC)"
	@cd contracts/ethereum && forge build

generate-verifier: ## Generate Halo2 Yul verifier and compile to bytecode
	@echo "$(BLUE)Generating Halo2 verifier...$(NC)"
	@chmod +x scripts/regenerate_verifier.sh
	@./scripts/regenerate_verifier.sh

generate-proof: ## Generate a test proof
	@echo "$(BLUE)Generating test proof...$(NC)"
	@cargo run --bin generate-proof -- 12345 67890 43981 1000 4660

test: ## Run all tests
	@echo "$(BLUE)Running tests...$(NC)"
	@chmod +x test.sh
	@./test.sh

test-rust: ## Run only Rust tests
	@echo "$(BLUE)Running Rust tests...$(NC)"
	@chmod +x test.sh
	@./test.sh --rust

test-solidity: bootstrap-foundry-deps ## Run only Solidity tests
	@echo "$(BLUE)Running Solidity tests...$(NC)"
	@chmod +x test.sh
	@./test.sh --solidity

test-verbose: ## Run tests with verbose output
	@echo "$(BLUE)Running tests (verbose)...$(NC)"
	@chmod +x test.sh
	@./test.sh --verbose

test-coverage: ## Generate test coverage report
	@echo "$(BLUE)Generating coverage report...$(NC)"
	@chmod +x test.sh
	@./test.sh --coverage

test-integration: ## Run integration tests (requires Anvil to be running)
	@echo "$(BLUE)Running integration tests...$(NC)"
	@echo "$(YELLOW)Note: Make sure Anvil is running (run 'make run-local' in another terminal)$(NC)"
	@cargo test --package eth-frontend --test integration_test -- --ignored

format: ## Format all code (Rust + Solidity)
	@echo "$(BLUE)Formatting code...$(NC)"
	@cargo fmt --all
	@cd contracts/ethereum && forge fmt

format-check: ## Check code formatting without modifying
	@echo "$(BLUE)Checking code formatting...$(NC)"
	@rustup run nightly cargo fmt --all -- --check
	@cd contracts/ethereum && forge fmt --check

lint: ## Run linters (clippy for Rust)
	@echo "$(BLUE)Running linters...$(NC)"
	@rustup run nightly cargo clippy --all-targets --all-features -- -D warnings

check: format-check lint test ## Run all checks (format, lint, test)

clean: ## Clean build artifacts
	@echo "$(BLUE)Cleaning build artifacts...$(NC)"
	@cargo clean
	@cd contracts/ethereum && forge clean
	@rm -rf coverage/

install: setup build ## Install dependencies and build project

watch: ## Watch for changes and rebuild
	@echo "$(BLUE)Watching for changes...$(NC)"
	@cargo watch -x check -x test

watch-test: ## Watch for changes and run tests
	@echo "$(BLUE)Watching for changes and running tests...$(NC)"
	@cargo watch -x test

run-local: ## Start local Ethereum node (Anvil)
	@echo "$(BLUE)Starting local Ethereum node...$(NC)"
	@anvil

deploy-local: ## Deploy contracts to local network
	@echo "$(BLUE)Deploying to local network...$(NC)"
	@cd contracts/ethereum && forge script script/Deploy.s.sol --rpc-url http://localhost:8545 --broadcast

docs: ## Generate documentation
	@echo "$(BLUE)Generating documentation...$(NC)"
	@cargo doc --workspace --no-deps --open

docs-solidity: ## Generate Solidity documentation
	@echo "$(BLUE)Generating Solidity documentation...$(NC)"
	@cd contracts/ethereum && forge doc

audit: ## Run security audit
	@echo "$(BLUE)Running security audit...$(NC)"
	@cargo audit
	@cd contracts/ethereum && forge audit

update: ## Update dependencies
	@echo "$(BLUE)Updating dependencies...$(NC)"
	@cargo update
	@cd contracts/ethereum && forge update

# Development helpers
dev-setup: setup ## Setup development environment
	@echo "$(BLUE)Setting up development environment...$(NC)"
	@cp .env.example .env || true
	@echo "$(GREEN)Development environment ready!$(NC)"
	@echo "$(YELLOW)Don't forget to configure .env file$(NC)"

ci: format-check lint test ## Run CI checks locally

# ────────────────────────────────────────────────────────────────────────────
# Coverage and pre-push targets — mirror what CI runs so red pipelines are
# easy to reproduce locally.
#
# Pipelines #5741 + #5744 (2026-05-20) both failed on patterns that pass
# `forge test` and `cargo test` locally but trip `forge coverage`:
#   - vm.assume rejection cap (fuzz test rejected > 65 536 inputs);
#   - Stack-too-deep (forge coverage disables optimizer + viaIR).
# Run `make pre-push` before pushing any non-trivial Solidity or Rust change
# to catch both classes locally.
# ────────────────────────────────────────────────────────────────────────────

coverage-solidity: ## Run forge coverage --report summary (matches test:solidity:coverage CI job)
	@echo "$(BLUE)Running forge coverage --report summary...$(NC)"
	@echo "$(YELLOW)Note: coverage disables optimizer; --ir-minimum avoids stack-too-deep.$(NC)"
	@cd contracts/ethereum && forge coverage --ir-minimum --report summary

relayer-test: ## Run bridge-relayer-daemon unit tests (via an-bridge-prover workspace)
	@echo "$(BLUE)Running bridge-relayer-daemon tests...$(NC)"
	@chmod +x scripts/bootstrap-cargo-git.sh
	@source ./scripts/bootstrap-cargo-git.sh && cd crates/an-bridge-prover && cargo test --locked -p bridge-relayer-daemon

aggregator-test: ## Run bridge-evm-aggregator tests (release, ~3 min)
	@echo "$(BLUE)Running bridge-evm-aggregator tests...$(NC)"
	@cd crates/bridge-evm-aggregator && cargo test --release --locked

generate-spike-artifacts: ## Export M2 multiply-spike verifier + calldata for Foundry (~5 min)
	@echo "$(BLUE)Generating R15 spike artefacts...$(NC)"
	@cd crates/bridge-evm-aggregator && cargo run --release --locked --bin export-spike-artifacts
	@chmod +x scripts/check_eip170_verifier_bins.sh
	@./scripts/check_eip170_verifier_bins.sh contracts/ethereum/test/fixtures/r15_spike
	@echo "$(GREEN)Spike artefacts written to contracts/ethereum/test/fixtures/r15_spike/$(NC)"

relayer-fmt: ## Check bridge-relayer-daemon formatting
	@cd crates/bridge-relayer-daemon && rustup run nightly cargo fmt --check

relayer-clippy: ## Run clippy on bridge-relayer-daemon (via an-bridge-prover workspace)
	@cd crates/an-bridge-prover && cargo clippy -p bridge-relayer-daemon --all-targets --no-deps -- -D warnings

production-preflight: ## Phase 0 gates before Sepolia/shellnet deploy (see docs/production_plan.md)
	@chmod +x scripts/production_preflight.sh
	@./scripts/production_preflight.sh

audit-solidity-test: ## Run audit overlay Foundry suite (47 tests @ profile audit)
	@echo "$(BLUE)Running audit/spec/ethereum (FOUNDRY_PROFILE=audit)...$(NC)"
	@cd contracts/ethereum && test -d lib/forge-std || forge install --no-git foundry-rs/forge-std
	@cd audit/spec/ethereum && FOUNDRY_PROFILE=audit forge test

audit-solidity-ci: ## Audit overlay @ profile ci (5000 fuzz / 1000 inv; ~8 min)
	@echo "$(BLUE)Running audit/spec/ethereum (FOUNDRY_PROFILE=ci)...$(NC)"
	@chmod +x scripts/ci_eth_audit_night.sh
	@./scripts/ci_eth_audit_night.sh

audit-deposit-relayer-test: ## F10 deposit-relayer unit + integration + proptest
	@echo "$(BLUE)Running deposit-relayer-daemon tests...$(NC)"
	@chmod +x scripts/check_deposit_audit_gates.sh
	@./scripts/check_deposit_audit_gates.sh
	@cd crates/deposit-relayer-daemon && cargo test

bootstrap-an-audit-py: ## Create .venv-an-audit with pytest + hypothesis
	@chmod +x scripts/bootstrap-an-audit-py.sh
	@./scripts/bootstrap-an-audit-py.sh

sync-audit-deposit-fixtures: ## Copy deposit_10proofs from deposit-prover to audit overlay
	@chmod +x scripts/sync_audit_deposit_fixtures.sh
	@./scripts/sync_audit_deposit_fixtures.sh

audit-an-test: ## Full AN pytest (unit+integration; sync fixtures first)
	@echo "$(BLUE)Running audit/spec/an (pytest)...$(NC)"
	@test -x .tools/tvm-debugger || $(MAKE) setup-an-audit-tools
	@test -x .venv-an-audit/bin/python || $(MAKE) bootstrap-an-audit-py
	@test -f audit/spec/an/fixtures/deposit_10proofs/proof_00/proof.bin || $(MAKE) sync-audit-deposit-fixtures
	@cd audit/spec/an-contracts && test -f build/USDCBridge.tvc || ./build.sh
	@.venv-an-audit/bin/python -m pytest -q audit/spec/an

setup-an-audit-tools: ## Symlink sold + tvm-debugger into .tools/
	@chmod +x scripts/setup_an_audit_tools.sh
	@./scripts/setup_an_audit_tools.sh

setup-audit-vendors: ## Shallow-clone partner repos into audit/vendors/ (gitignored)
	@chmod +x scripts/setup_audit_vendors.sh
	@./scripts/setup_audit_vendors.sh

pre-push-an: ## AN audit gate: unit pytest (no fixtures required)
	@echo "$(BLUE)── pre-push-an: AN audit gate ──$(NC)"
	@$(MAKE) setup-an-audit-tools
	@test -x .venv-an-audit/bin/python || $(MAKE) bootstrap-an-audit-py
	@.venv-an-audit/bin/python -m pytest -q audit/spec/an/unit
	@echo "$(GREEN)── pre-push-an: green ──$(NC)"

pre-push-audit: ## Audit branch gate: fmt + main forge test + audit overlay + AN unit
	@echo "$(BLUE)── pre-push-audit: ETH audit closeout gate ──$(NC)"
	@$(MAKE) bootstrap-foundry-deps
	@cd contracts/ethereum && forge fmt --check
	@cd contracts/ethereum && forge test
	@$(MAKE) audit-solidity-test
	@$(MAKE) pre-push-an
	@echo "$(GREEN)── pre-push-audit: green ──$(NC)"

pre-push: ## Mirror CI: format-check + clippy + tests + Solidity coverage. Run before `git push`.
	@echo "$(BLUE)── pre-push: mirroring CI ──$(NC)"
	@$(MAKE) format-check
	@$(MAKE) lint
	@$(MAKE) relayer-fmt
	@$(MAKE) relayer-clippy
	@cd contracts/ethereum && forge fmt --check
	@cd contracts/ethereum && forge test
	@$(MAKE) coverage-solidity
	@cargo test --workspace --locked
	@$(MAKE) relayer-test
	@$(MAKE) aggregator-test
	@chmod +x scripts/check_eip170_verifier_bins.sh
	@./scripts/check_eip170_verifier_bins.sh contracts/ethereum/test/fixtures/r15_spike 2>/dev/null || \
	 ./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers 2>/dev/null || true
	@chmod +x scripts/check_withdrawal_verifier_not_stub.sh
	@./scripts/check_withdrawal_verifier_not_stub.sh
	@echo "$(GREEN)── pre-push: all green; safe to push ──$(NC)"

# Quick commands
q-build: ## Quick build (debug mode)
	@cargo build --workspace

q-test: ## Quick test (no verbose)
	@cargo test --workspace

q-check: ## Quick check (no build)
	@cargo check --workspace

# Utility commands
tree: ## Show project structure
	@tree -I 'target|node_modules|lib|out|coverage' -L 3

size: ## Show build artifact sizes
	@echo "$(BLUE)Build artifact sizes:$(NC)"
	@du -sh target/debug target/release 2>/dev/null || echo "No build artifacts found"
	@du -sh contracts/ethereum/out 2>/dev/null || echo "No Solidity artifacts found"

info: ## Show project information
	@echo "$(BLUE)Project Information$(NC)"
	@echo "Rust version:     $$(rustc --version)"
	@echo "Cargo version:    $$(cargo --version)"
	@if command -v forge >/dev/null 2>&1; then \
		echo "Forge version:    $$(forge --version | head -n 1)"; \
		echo "Anvil version:    $$(anvil --version)"; \
	else \
		echo "Forge:            $(YELLOW)Not installed (run 'make setup')$(NC)"; \
	fi
	@echo "Project root:     $$(pwd)"

