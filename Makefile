.PHONY: help setup build test clean format lint check install run-local deploy docs \
        coverage-solidity pre-push production-preflight relayer-test relayer-fmt relayer-clippy aggregator-fmt \
        english-check

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

build-solidity: ## Build only Solidity contracts
	@echo "$(BLUE)Building Solidity contracts...$(NC)"
	@cd contracts/ethereum && forge build

test: ## Run all tests
	@echo "$(BLUE)Running tests...$(NC)"
	@chmod +x test.sh
	@./test.sh

test-rust: ## Run only Rust tests
	@echo "$(BLUE)Running Rust tests...$(NC)"
	@chmod +x test.sh
	@./test.sh --rust

test-solidity: ## Run only Solidity tests
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

# Crates outside the root workspace that are packages of their own. The relayer
# and the withdrawal CLI are members of crates/bridge-prover-libraries and are
# formatted through it.
STANDALONE_CRATES := deposit-prover eth-light-client-prover frontend \
	crates/bridge-evm-aggregator crates/bridge-snark-utils \
	crates/deposit-relayer-daemon crates/eth-light-client-relayer

format: ## Format all code (every Rust crate + Solidity)
	@echo "$(BLUE)Formatting code...$(NC)"
	@cargo fmt --all
	@cd crates/bridge-prover-libraries && cargo fmt --all
	@for d in $(STANDALONE_CRATES); do (cd $$d && cargo fmt) || exit 1; done
	@cd contracts/ethereum && forge fmt

format-check: ## Check code formatting without modifying
	@echo "$(BLUE)Checking code formatting...$(NC)"
	@cargo fmt --all -- --check
	@cd contracts/ethereum && forge fmt --check

lint: ## Run linters (clippy for Rust)
	@echo "$(BLUE)Running linters...$(NC)"
	@cargo clippy --all-targets --all-features -- -D warnings

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

deploy-local: ## Deploy a test bridge to Anvil (PRIVATE_KEY from the environment or contracts/ethereum/.env)
	@echo "$(BLUE)Deploying to local network...$(NC)"
	@cd contracts/ethereum && forge script script/DeployTestBridge.s.sol --rpc-url http://localhost:8545 --broadcast

docs: ## Generate documentation
	@echo "$(BLUE)Generating documentation...$(NC)"
	@cargo doc --workspace --no-deps --open

docs-solidity: ## Generate Solidity documentation
	@echo "$(BLUE)Generating Solidity documentation...$(NC)"
	@cd contracts/ethereum && forge doc

audit: ## Run cargo audit on the root workspace
	@echo "$(BLUE)Running security audit...$(NC)"
	@cargo audit

update: ## Update dependencies
	@echo "$(BLUE)Updating dependencies...$(NC)"
	@cargo update
	@cd contracts/ethereum && forge update

# Development helpers
dev-setup: setup ## Setup development environment
	@echo "$(BLUE)Setting up development environment...$(NC)"
	@test -f contracts/ethereum/.env || cp contracts/ethereum/.env.example contracts/ethereum/.env
	@echo "$(GREEN)Development environment ready!$(NC)"
	@echo "$(YELLOW)Don't forget to configure contracts/ethereum/.env$(NC)"

ci: format-check lint test ## Run CI checks locally

# ────────────────────────────────────────────────────────────────────────────
# Coverage and pre-push targets. No pipeline on GitHub runs Rust or
# `forge coverage`, so `make pre-push` is the gate for both; see the CI section
# of AGENTS.md.
#
# Two patterns pass `forge test` and `cargo test` but trip `forge coverage`:
#   - vm.assume rejection cap (fuzz test rejected > 65 536 inputs);
#   - Stack-too-deep (forge coverage disables optimizer + viaIR).
# Run `make pre-push` before pushing any non-trivial Solidity or Rust change
# to catch both classes locally.
# ────────────────────────────────────────────────────────────────────────────

coverage-solidity: ## Run forge coverage --report summary (matches test:solidity:coverage CI job)
	@echo "$(BLUE)Running forge coverage --report summary...$(NC)"
	@echo "$(YELLOW)Note: coverage disables optimizer + viaIR; expect Stack-too-deep here$(NC)"
	@echo "$(YELLOW)      if any function has > 16 live local stack slots.$(NC)"
	@cd contracts/ethereum && forge coverage --report summary

relayer-test: ## Run bridge-relayer-daemon unit tests (via bridge-prover-libraries workspace)
	@echo "$(BLUE)Running bridge-relayer-daemon tests...$(NC)"
	@cd crates/bridge-prover-libraries && cargo test --locked -p bridge-relayer-daemon

aggregator-test: ## Run bridge-evm-aggregator tests (release, ~3 min)
	@echo "$(BLUE)Running bridge-evm-aggregator tests...$(NC)"
	@cd crates/bridge-evm-aggregator && cargo test --release --locked

generate-spike-artifacts: ## Export M2 multiply-spike verifier + calldata for Foundry (~5 min)
	@echo "$(BLUE)Generating R15 spike artefacts...$(NC)"
	@cd crates/bridge-evm-aggregator && cargo run --release --locked --bin export-spike-artifacts
	@chmod +x scripts/check_eip170_verifier_bins.sh
	@./scripts/check_eip170_verifier_bins.sh contracts/ethereum/test/fixtures/r15_spike
	@echo "$(GREEN)Spike artefacts written to contracts/ethereum/test/fixtures/r15_spike/$(NC)"

relayer-fmt: ## Check bridge-relayer-daemon formatting (via bridge-prover-libraries workspace)
	@cd crates/bridge-prover-libraries && cargo fmt -p bridge-relayer-daemon -- --check

aggregator-fmt: ## Check bridge-evm-aggregator formatting
	@cd crates/bridge-evm-aggregator && cargo fmt --check

relayer-clippy: ## Run clippy on bridge-relayer-daemon (via bridge-prover-libraries workspace)
	@cd crates/bridge-prover-libraries && cargo clippy -p bridge-relayer-daemon --all-targets --no-deps -- -D warnings

production-preflight: ## Phase 0 gates before Sepolia/shellnet deploy (gates live in scripts/production_preflight.sh)
	@chmod +x scripts/production_preflight.sh
	@./scripts/production_preflight.sh

english-check: ## Check that every tracked file is English-only (matches the Woodpecker `english` step)
	@python3 scripts/check_english_only.py

pre-push: ## Mirror CI: format-check + clippy + tests + Solidity coverage. Run before `git push`.
	@echo "$(BLUE)── pre-push: mirroring CI ──$(NC)"
	@$(MAKE) english-check
	@$(MAKE) format-check
	@$(MAKE) lint
	@$(MAKE) relayer-fmt
	@$(MAKE) aggregator-fmt
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

