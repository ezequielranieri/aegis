# Skill Registry — aegis

Generated: 2026-09-06
Project: aegis
Workspace: /home/ez/Projects/aegis
Source: /home/ez/.config/opencode/skills (user-level)

## Registry Contract

This is an **index**, not a summary. Each entry points to the authoritative `SKILL.md`. Subagents receive exact paths and read the full skill source of truth.

- `scope`: `global` = user-level skill from `~/.config/opencode/skills/`
- `scope`: `project` = project-level skill from `{project-root}/skills/` or similar
- Duplicate skill names are deduplicated; project-level wins over global
- Skipped: `sdd-*`, `_shared`, `skill-registry`

## Skills Index

| Skill Name | Trigger / Description | Scope | Path |
|---|---|---|---|
| agent-authorization-loop | policy-aware agent loop, PEP before tool call, childproofing control plane, TPE guided planning, delegation as data, forbid guardrail, subagent never more authority. Core pattern for agent-gateway delegation and mettle security evals. | global | /home/ez/.config/opencode/skills/agent-authorization-loop/SKILL.md |
| agent-evals | agent evaluation, evals, golden set, LLM as judge, prompt injection, tool abuse, agent security, regression, mettle. Specification is the oracle: deterministic checks first, LLM judge for ambiguity only. | global | /home/ez/.config/opencode/skills/agent-evals/SKILL.md |
| ai-engineering-guardrails | PII mask/unmask, false refusal rate, router gateway separation, cache tenant_id, MTTD MTTR CFR, drift detection, system prompt hash. Runtime guardrails for multi-tenant LLM gateway. | global | /home/ez/.config/opencode/skills/ai-engineering-guardrails/SKILL.md |
| branch-pr | Create Gentle AI pull requests with issue-first checks. Trigger: creating, opening, or preparing PRs for review. | global | /home/ez/.config/opencode/skills/branch-pr/SKILL.md |
| chained-pr | PRs over 400 lines, stacked PRs, review slices. Split oversized changes into chained PRs that protect review focus. | global | /home/ez/.config/opencode/skills/chained-pr/SKILL.md |
| cognitive-doc-design | Design docs that reduce cognitive load. Trigger: writing guides, READMEs, RFCs, onboarding, architecture, or review-facing docs. | global | /home/ez/.config/opencode/skills/cognitive-doc-design/SKILL.md |
| comment-writer | Write warm, direct collaboration comments. Trigger: PR feedback, issue replies, reviews, Slack messages, or GitHub comments. | global | /home/ez/.config/opencode/skills/comment-writer/SKILL.md |
| crypto-receipts | execution receipts, signed receipts, hash-chain, merkle tree, verifiable execution, ed25519, blake3. Produce and verify cryptographic proof of WASM execution. | global | /home/ez/.config/opencode/skills/crypto-receipts/SKILL.md |
| ddia-distributed-systems-patterns | exactly-once, idempotency, linearizability, saga, 2PC, fencing, isolation level, write skew, replication lag, B-tree, LSM, stream processing, schema evolution. Apply DDIA patterns to durable queues, workflows, multi-tenant gateways, auth services. | global | /home/ez/.config/opencode/skills/ddia-distributed-systems-patterns/SKILL.md |
| decision-log | DECISIONS.md, ADR, decision record, known issues, retrospectiva, trade-offs, what is not implemented. Maintain honest documentation: numbered AD entries, known-issue classification, and explicit non-implementation lists. | global | /home/ez/.config/opencode/skills/decision-log/SKILL.md |
| declarative-policy-engine | policy engine, OPA, Cedar, rego, declarative policy, host function evaluation, policy versioning. Evaluate capability policies in WASM host functions with versioned schema. | global | /home/ez/.config/opencode/skills/declarative-policy-engine/SKILL.md |
| gentle-ai-bench | bench, journey, journeys, driven mode, gentle-ai-bench, journey corpus, j-numbers, bench axis. Author and verify gentle-ai bench journeys; go test ./bench never proves driven execution. | global | /home/ez/.config/opencode/skills/gentle-ai-bench/SKILL.md |
| go-hexagonal-security | clean architecture, hexagonal, ports and adapters, Go service, domain layer, use cases, JWT, refresh token. Structure Go services as hexagonal architecture with pure domain, port-only use cases, adapters, and pinned crypto invariants. | global | /home/ez/.config/opencode/skills/go-hexagonal-security/SKILL.md |
| go-testing | Go tests, go test coverage, Bubbletea teatest, golden files. Apply focused Go testing patterns. | global | /home/ez/.config/opencode/skills/go-testing/SKILL.md |
| issue-creation | issue creation, bug reports, feature requests, or issue approval. Create and triage GitHub issues from repository evidence. | global | /home/ez/.config/opencode/skills/issue-creation/SKILL.md |
| judgment-day | judgment day, dual review, adversarial review, juzgar. Run explicit blind dual review with at most two scoped fix/re-judgment rounds. | global | /home/ez/.config/opencode/skills/judgment-day/SKILL.md |
| multi-tenant-authorization-two-layer | two-layer authz, platform vs tenant, tenant_id first class, OIDC federated hosted, device posture staff only, PDP shared dedicated, who-can-access 3-step. Blueprint for agro-iam multi-tenant authorization. | global | /home/ez/.config/opencode/skills/multi-tenant-authorization-two-layer/SKILL.md |
| observability-engineering | observability, OpenTelemetry, SLO, SLI, cardinality, core analysis loop, sampling, noisy neighbor, wide events. Implement vendor-neutral observability with high-cardinality events, event-based SLOs, and systematic debugging. | global | /home/ez/.config/opencode/skills/observability-engineering/SKILL.md |
| postgres-rls-tenancy | SQL RLS, row level security, multi-tenancy, tenant isolation, cross-tenant, set_config, FORCE. Apply canonical Postgres RLS tenancy with global tenant tables, tenant-scoped policies, and local set_config binding. | global | /home/ez/.config/opencode/skills/postgres-rls-tenancy/SKILL.md |
| rdd-defect-workflow | RDD, receipt-driven development, review authority, receipt/lineage, correction/recovery, delivery gate/kill switch, bounded review defects. Guide work. | global | /home/ez/.config/opencode/skills/rdd-defect-workflow/SKILL.md |
| rust-wasmtime-sandbox | Rust, Wasmtime, WASI, wasm sandbox, capability-based security, host functions. Build secure WASM runtime with explicit capabilities, fail-closed sandboxing, and resource limits. | global | /home/ez/.config/opencode/skills/rust-wasmtime-sandbox/SKILL.md |
| secret-hygiene | SOPS, age, gitleaks, secrets, .env, credential rotation, leak. Encrypt secrets with SOPS + age, scan with gitleaks in CI, and rotate fully on any suspected leak. | global | /home/ez/.config/opencode/skills/secret-hygiene/SKILL.md |
| skill-creator | new skills, agent instructions, documenting AI usage patterns. Create LLM-first skills with valid frontmatter. | global | /home/ez/.config/opencode/skills/skill-creator/SKILL.md |
| skill-improver | improve skills, audit skills, refactor skills, skill quality. Audit and upgrade existing LLM-first skills. | global | /home/ez/.config/opencode/skills/skill-improver/SKILL.md |
| systemic-issue-triage | new issue, bug report, triage, backlog, issue flood, community report, root cause, dead-end, blocked user. Attack issues by root class, never one-by-one; fixes must shrink the system, not grow it. | global | /home/ez/.config/opencode/skills/systemic-issue-triage/SKILL.md |
| work-unit-commits | Plan commits as reviewable work units. Trigger: implementation, commit splitting, chained PRs, or keeping tests and docs with code. | global | /home/ez/.config/opencode/skills/work-unit-commits/SKILL.md |

## Project Convention Files

| File | Path | References |
|---|---|---|
| AGENTS.md | /home/ez/Projects/aegis/AGENTS.md | (not found) |
| CLAUDE.md | /home/ez/Projects/aegis/CLAUDE.md | (not found) |
| .cursorrules | /home/ez/Projects/aegis/.cursorrules | (not found) |
| GEMINI.md | /home/ez/Projects/aegis/GEMINI.md | (not found) |
| copilot-instructions.md | /home/ez/Projects/aegis/copilot-instructions.md | (not found) |

## Summary

- **Total indexed skills**: 25
- **Global skills**: 25
- **Project skills**: 0
- **Skipped (sdd-*)**: 8 (sdd-apply, sdd-archive, sdd-design, sdd-explore, sdd-init, sdd-onboard, sdd-propose, sdd-research, sdd-spec, sdd-tasks, sdd-verify)
- **Skipped (_shared)**: 1
- **Skipped (skill-registry)**: 1
- **Engram persistence**: Available (will save with topic_key: skill-registry)