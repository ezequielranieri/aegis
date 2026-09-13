# Network HTTP Specification (Phase 7)

## Purpose

Host function `aegis::http_fetch` gives WASM modules TLS-only HTTPS access to allowlisted hosts with method allowlist, per-execution rate limit, and 1 MiB response cap. Violations trap fail-closed with signed receipts (REQ-107). Reinstates ADR-013 #1.

## Requirements

| Req | Statement | Severity |
|-----|-----------|----------|
| REQ-601 | `allowed_methods` defaults `["GET"]`; `SandboxState` holds `Option<NetworkHttpParams>`; `CapabilityConfig` untouched | MUST |
| REQ-602 | Config validation fail-closed | MUST |
| REQ-603 | Host fn contract with validation order; violations trap | MUST |
| REQ-604 | https/443/case-insensitive hostname; else S-601 | MUST |
| REQ-605 | Method allowlist; else S-602 | MUST |
| REQ-606 | Token bucket; empty → S-606 | MUST |
| REQ-607 | TLS-only; S-603 DNS/connect; S-604 2s/5s | MUST |
| REQ-608 | 1 MiB cap; else S-605 | MUST |
| REQ-609 | Receipt per attempt; schema unchanged | MUST |
| REQ-610 | Execute path = fetched URL, result = BLAKE3(body), size = response bytes; traps→FAILED_PRECONDITION | MUST |

### Requirement: REQ-601 — Network Configuration

`allowed_methods` SHALL default `["GET"]`; policy SHALL travel via `SandboxState.network_http`.

#### Scenario: Default methods

- GIVEN no `allowed_methods` in config; WHEN loaded; THEN `["GET"]` used.

### Requirement: REQ-602 — Fail-Closed Config Validation

Validation SHALL reject empty `allowed_hosts`, IP-literal hosts, empty `allowed_methods`, zero `max_requests_per_second`.

#### Scenario: Invalid configs

- GIVEN empty `allowed_hosts` or `allowed_methods`; WHEN config loads; THEN fails.

### Requirement: REQ-603 — Host Function Contract

`aegis_http_fetch(method_ptr, method_len, url_ptr, url_len, out_ptr, out_len)` SHALL validate endpoint→method→rate→fetch→size; violations SHALL trap (REQ-107).

#### Scenario: Happy fetch

- GIVEN allowed host, `GET` allowed, bucket free; WHEN fetch `https://example.com/x`; THEN body copied, length returned.

### Requirement: REQ-604 — Endpoint Allowlist (S-601)

Endpoint SHALL be `https://`, port 443, hostname exact case-insensitive in `allowed_hosts`; else trap S-601.

#### Scenario: Case-insensitive

- GIVEN allowlist `example.com`; WHEN fetch `https://EXAMPLE.COM/x`; THEN passes.

### Requirement: REQ-605 — Method Allowlist (S-602)

Method SHALL be in `allowed_methods`; else trap S-602.

#### Scenario: Disallowed

- GIVEN `allowed_methods = ["GET"]`; WHEN fetch with `POST`; THEN trap S-602.

### Requirement: REQ-606 — Rate Limit (S-606)

Fetch SHALL consume one token; empty bucket traps S-606.

#### Scenario: Burst

- GIVEN bucket = 2; WHEN third rapid call arrives; THEN trap S-606.

### Requirement: REQ-607 — TLS and Timeouts (S-603, S-604)

Fetch SHALL be TLS-only; DNS/connect fails → S-603; connect > 2s or total > 5s → S-604.

#### Scenario: Transport

- GIVEN unresolvable host or stalling server; WHEN fetch; THEN trap S-603 or S-604.

### Requirement: REQ-608 — Response Cap (S-605)

Response SHALL be ≤ 1,048,576 bytes; larger traps S-605.

#### Scenario: Boundary

- GIVEN body exactly 1 MiB; WHEN fetch; THEN succeeds (1 MiB + 1 byte traps S-605).

### Requirement: REQ-609 — Fetch Receipts

Every attempt SHALL emit `network.http`/`fetch`/<URL>/len/`blake3(body)`|`trap`; schema unchanged.

#### Scenario: Emission

- GIVEN success or violation; WHEN host fn returns or traps; THEN receipt appended, chain intact.

### Requirement: REQ-610 — gRPC Wiring

Execute SHALL report `path` = URL of the fetch actually performed (`scheme://host/path`), `result` = BLAKE3(body), `size` = response byte length; network traps map to FAILED_PRECONDITION, success=false.

#### Scenario: Mapping

- GIVEN execute with `network.http`; WHEN done; THEN path is the fetched URL, traps→FAILED_PRECONDITION.

## Scenarios

| # | Scenario | Then |
|---|----------|------|
| S-601 | Endpoint outside policy (scheme/port/hostname/IP) | Trap + trap receipt |
| S-602 | Method outside `allowed_methods` | Trap + trap receipt |
| S-603 | DNS/connect failure | Trap + trap receipt |
| S-604 | Exceeds 2s connect / 5s total | Trap + trap receipt |
| S-605 | Body > 1 MiB | Trap + trap receipt |
| S-606 | Bucket exhausted | Trap + trap receipt |
| E-601 | Empty hosts / empty methods config | Load fails |
| E-602 | `https://EXAMPLE.COM/x` vs `example.com` | Succeeds |
| E-603 | `https://example.com:8443/x` | Trap S-601 |
| E-604 | Body exactly 1 MiB | Succeeds |
| E-605 | 3 calls, bucket = 2 | 2 succeed, 1 traps S-606 |