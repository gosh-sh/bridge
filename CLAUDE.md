# CLAUDE.md

The context and instructions for this repository live in [AGENTS.md](AGENTS.md).
Read it in full before making changes — including the documentation-status
warning at the top: everything outside `docs/archive/` is current, the archive
is not, and when code and prose disagree the code wins.

The changelog rule, in short: every branch opened as a PR into `main` describes
its diff against `main` in [`CHANGELOG.md`](CHANGELOG.md). Write for a devops
engineer or a developer who deploys the bridge and runs the relayers — contract
functions and events, circuit public inputs and verification keys, relayer and
prover CLI, config and env vars, scripts, deployment steps. A rotated
verification key is always a breaking change. Skip internal refactors.

Release numbers are assigned only when a release is tagged, so entries go under
`## [Unreleased]` at the top of the changelog. Do not bump `version` in any
`Cargo.toml` and do not invent a version heading — a human does that at release
time. Never edit or append to a section of an already released version.
