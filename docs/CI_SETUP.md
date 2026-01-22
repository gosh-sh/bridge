# CI/CD Setup

This document explains the GitLab CI/CD pipeline configuration for the Acki Nacki Bridge project.

## Overview

The CI pipeline is configured in `.gitlab-ci.yml` and consists of 5 stages:

1. **Setup**: Install dependencies and prepare caches
2. **Build**: Build Rust and Solidity code
3. **Test**: Run unit tests, integration tests, and linting
4. **Security**: Run security audits and static analysis
5. **Deploy**: Deploy to testnet/mainnet (manual)

## Docker Images

### Rust Jobs

- **Image**: `rust:latest`
- **Components**: rustfmt, clippy
- **Purpose**: Build and test Rust workspace

### Solidity Jobs

- **Image**: `ghcr.io/foundry-rs/foundry:latest`
- **Additional**: Node.js and npm (for poseidon-solidity dependency)
- **Purpose**: Build and test Solidity contracts

## Key Configuration

### Node.js Installation

The Foundry Docker image doesn't include Node.js by default, but we need it for the `poseidon-solidity` npm package. We install it in the `before_script`:

```yaml
.foundry_base:
  image: ghcr.io/foundry-rs/foundry:latest
  before_script:
    # Install Node.js and npm for poseidon-solidity dependency
    - apt-get update && apt-get install -y nodejs npm
    - node --version
    - npm --version
    - forge --version
    - cast --version
```

### NPM Dependencies

Every Solidity job installs npm dependencies before running:

```yaml
script:
  - cd contracts/ethereum
  # Install npm dependencies (poseidon-solidity)
  - npm install
  # Run forge command
  - forge build
```

### Caching

We cache three types of data:

1. **Rust cache** (`rust-$CI_COMMIT_REF_SLUG`):
   - `.cargo/` - Cargo registry and git dependencies
   - `target/` - Compiled Rust artifacts

2. **Foundry cache** (`foundry-$CI_COMMIT_REF_SLUG`):
   - `contracts/ethereum/out/` - Compiled Solidity artifacts
   - `contracts/ethereum/cache/` - Forge cache

3. **NPM cache** (`foundry-npm-$CI_COMMIT_REF_SLUG`):
   - `contracts/ethereum/node_modules/` - NPM packages (poseidon-solidity)
   - `contracts/ethereum/out/` - Compiled Solidity artifacts
   - `contracts/ethereum/cache/` - Forge cache

## Stages

### 1. Setup Stage

#### `setup:rust`
- Fetches Rust dependencies
- Only runs when Cargo files change
- Populates Rust cache

#### `setup:foundry`
- Installs npm dependencies (`poseidon-solidity`)
- Installs forge dependencies (`forge-std`)
- Only runs when foundry.toml, package.json, or remappings change
- Populates Foundry and NPM caches

### 2. Build Stage

#### `build:rust:debug`
- Builds Rust workspace in debug mode
- Runs on every commit
- Produces debug artifacts (1 day expiry)

#### `build:rust:release`
- Builds Rust workspace in release mode
- Only runs on `main` branch and tags
- Produces release artifacts (1 week expiry)

#### `build:solidity`
- Installs npm dependencies
- Builds Solidity contracts with Forge
- Produces compiled artifacts (1 day expiry)

### 3. Test Stage

#### Rust Tests

**`test:rust:unit`**
- Runs unit tests: `cargo test --workspace --lib`
- Reports coverage

**`test:rust:integration`**
- Runs integration tests: `cargo test --workspace --test '*'`

**`test:rust:doc`**
- Runs documentation tests: `cargo test --workspace --doc`

#### Solidity Tests

**`test:solidity`**
- Installs npm dependencies
- Runs Forge tests: `forge test -vvv`
- Produces JUnit test results

**`test:solidity:coverage`**
- Installs npm dependencies
- Runs coverage: `forge coverage --report summary`
- Reports coverage percentage
- Only runs on `main` and merge requests

#### Linting

**`lint:rust:fmt`**
- Checks Rust formatting: `cargo fmt --all -- --check`
- Fails pipeline if formatting is incorrect

**`lint:rust:clippy`**
- Runs Clippy linter: `cargo clippy --all-targets --all-features -- -D warnings`
- Fails pipeline if warnings are found

**`lint:solidity:fmt`**
- Installs npm dependencies
- Checks Solidity formatting: `forge fmt --check`
- Fails pipeline if formatting is incorrect

### 4. Security Stage

#### `security:rust:audit`
- Runs `cargo audit` to check for known vulnerabilities
- Only runs on `main` and merge requests
- Allowed to fail (won't block pipeline)

#### `security:solidity:slither`
- Runs Slither static analyzer
- Uses `trailofbits/eth-security-toolbox` image
- Only runs on `main` and merge requests
- Allowed to fail (won't block pipeline)

### 5. Deploy Stage

#### Documentation

**`docs:rust`**
- Generates Rust documentation: `cargo doc --workspace --no-deps`
- Produces documentation artifacts (1 week expiry)
- Only runs on `main`

**`docs:solidity`**
- Installs npm dependencies
- Generates Solidity documentation: `forge doc`
- Produces documentation artifacts (1 week expiry)
- Only runs on `main`

#### Deployment

**`deploy:testnet`**
- Installs npm dependencies
- Deploys to testnet using Forge script
- Manual trigger only
- Only runs on `main`
- Requires `$TESTNET_RPC_URL` environment variable

**`deploy:mainnet`**
- Installs npm dependencies
- Deploys to mainnet using Forge script
- Manual trigger only
- Only runs on tags
- Requires `$MAINNET_RPC_URL` environment variable

## Environment Variables

### Required for CI

- `CI_PROJECT_DIR` - GitLab CI built-in variable
- `CI_COMMIT_REF_SLUG` - GitLab CI built-in variable

### Required for Deployment

- `TESTNET_RPC_URL` - RPC URL for testnet deployment
- `MAINNET_RPC_URL` - RPC URL for mainnet deployment

### Optional

- `RUST_BACKTRACE=1` - Enable Rust backtraces (already set)
- `FOUNDRY_PROFILE=ci` - Use CI profile for Foundry (already set)

## Troubleshooting

### Issue: "poseidon-solidity not found"

**Cause**: npm dependencies not installed

**Solution**: Ensure `npm install` is run before any forge command:

```yaml
script:
  - cd contracts/ethereum
  - npm install  # Add this line
  - forge build
```

### Issue: "forge: command not found"

**Cause**: Not using Foundry Docker image

**Solution**: Ensure job extends `.foundry_base`:

```yaml
my_job:
  extends: .foundry_base  # Add this line
  script:
    - forge build
```

### Issue: "node: command not found"

**Cause**: Node.js not installed in Foundry image

**Solution**: Already handled in `.foundry_base` before_script. If you create a custom job, make sure to extend `.foundry_base`.

### Issue: Cache not working

**Cause**: Cache key mismatch or cache policy incorrect

**Solution**: Use consistent cache keys:

```yaml
cache:
  key: foundry-npm-$CI_COMMIT_REF_SLUG
  paths:
    - contracts/ethereum/node_modules/
  policy: pull  # or pull-push for jobs that modify cache
```

### Issue: Tests fail with "out of memory"

**Cause**: Merkle tree tests are computationally intensive

**Solution**: Increase GitLab Runner memory limit or use a more powerful runner.

## Best Practices

### 1. Always Install NPM Dependencies

Every Solidity job should install npm dependencies:

```yaml
script:
  - cd contracts/ethereum
  - npm install
  - forge <command>
```

### 2. Use Caching Effectively

- Use `policy: pull-push` for jobs that modify cache (setup, build)
- Use `policy: pull` for jobs that only read cache (test, lint)

### 3. Minimize Docker Image Pulls

- Extend base jobs (`.rust_base`, `.foundry_base`) instead of defining images directly
- This ensures consistent configuration across jobs

### 4. Use Artifacts for Build Outputs

- Build jobs should produce artifacts
- Test jobs should depend on build jobs and use artifacts
- This avoids rebuilding in every job

### 5. Run Security Checks on Important Branches

- Use `only: [main, merge_requests]` for security jobs
- This balances security with CI speed

## Local Testing

To test the CI pipeline locally, you can use GitLab Runner:

```bash
# Install GitLab Runner
curl -L https://packages.gitlab.com/install/repositories/runner/gitlab-runner/script.deb.sh | sudo bash
sudo apt-get install gitlab-runner

# Run a specific job
gitlab-runner exec docker build:solidity
```

Or test manually with Docker:

```bash
# Test Rust build
docker run --rm -v $(pwd):/workspace -w /workspace rust:latest bash -c "
  rustup component add rustfmt clippy
  cargo build --workspace
  cargo test --workspace
"

# Test Solidity build
docker run --rm -v $(pwd):/workspace -w /workspace ghcr.io/foundry-rs/foundry:latest bash -c "
  apt-get update && apt-get install -y nodejs npm
  cd contracts/ethereum
  npm install
  forge build
  forge test
"
```

## Performance Optimization

### Current Pipeline Duration

- **Setup**: ~30 seconds (with cache)
- **Build**: ~2 minutes (Rust) + ~30 seconds (Solidity)
- **Test**: ~5 minutes (Rust unit) + ~30 seconds (Solidity)
- **Total**: ~8-10 minutes

### Optimization Tips

1. **Use cache effectively**: Ensure all jobs use appropriate caches
2. **Parallelize tests**: Rust tests already run in parallel
3. **Skip unnecessary jobs**: Use `only` and `except` to skip jobs when possible
4. **Use faster runners**: Consider using runners with more CPU/memory

## References

- [GitLab CI/CD Documentation](https://docs.gitlab.com/ee/ci/)
- [Foundry Book - CI](https://book.getfoundry.sh/config/continuous-integration)
- [Cargo Book - CI](https://doc.rust-lang.org/cargo/guide/continuous-integration.html)

