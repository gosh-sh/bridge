# Acki Nacki Bridge — правила для агентов

Краткий вход. Детали — в `.cursor/rules/` и `docs/README.md`. Исторический монолит: `_archive/AGENTS.md`.

## Проект

Двусторонний мост ETH ↔ [Acki Nacki](https://docs.ackinacki.com/) на ZK-доказательствах. Репозиторий: контракты L1, relayer'ы, deposit-prover, документация. TVM-контракты — в `../acki-nacki`.

## Правила Cursor (`.cursor/rules/`)

| Файл | Когда |
|------|--------|
| `bridge-core-audit.mdc` | Всегда — BC/QC/OK, антипаттерны LLM, язык |
| `bridge-ethereum-audit.mdc` | `contracts/ethereum/**` |
| `bridge-an-zk-audit.mdc` | ZK, relayers, cross-chain |
| `bridge-docs-discipline.mdc` | `docs/`, README |
| `external-ssh.mdc` | Тяжёлые сборки → n14 |
| `dual-remote-git.mdc` | Push по запросу → origin + github |

## Методология аудита

Заимствовано из **ammalgam** (Foundry, fuzz, findings) и **dex** (TVM-слои, «не подгонять тесты»):

1. Падающий тест → классификация **BC / QC / OK** до любых правок
2. **BC** только с воспроизводящим тестом в `audit/findings/BRIDGE-XXX/`
3. Инварианты: `docs/operations/bridge_verification.md` (DEP-#, CC-#, …)
4. Факты домена: `audit/PROJECT_FACTS.md`

## Быстрые команды

```bash
cd contracts/ethereum && forge test
make pre-push                    # перед push (coverage + clippy + fmt)
cd crates/bridge-relayer-daemon && cargo test
cd crates/deposit-relayer-daemon && cargo test
```

## Документация

- Индекс: [docs/README.md](docs/README.md)
- Архитектура: [docs/architecture/four_circuit_architecture.md](docs/architecture/four_circuit_architecture.md)
- E2E deposit: skill `.cursor/skills/evm-an-deposit-e2e/SKILL.md`

## Git remotes

| Remote | URL |
|--------|-----|
| `origin` | `git@vcs.modus-ponens.com:ton/acki-nacki-bridge.git` |
| `github` | `git@github.com:gosh-sh/bridge-EVM.git` |
