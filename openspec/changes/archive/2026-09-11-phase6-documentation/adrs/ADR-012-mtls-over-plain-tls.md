# ADR-012: mTLS over Plain TLS

**Date**: 2026-09-11
**Phase**: 6 (documentation)
**Status**: Accepted

### Context
The gRPC boundary requires TLS for transport security. The project chose mutual TLS (mTLS) with client certificate validation over plain TLS (server-only authentication).

### Decision
Require mTLS with CN/SAN client certificate validation (`AegisClientCertVerifier`) for all gRPC connections.

### Rationale
- **Mutual authentication**: Plain TLS only authenticates the server to the client. mTLS authenticates both parties — the server presents its cert, the client must present a cert signed by the configured CA with CN/SAN matching `expected_identity`. This is essential for the agent-gateway (Go) ↔ aegis-runtime (Rust) boundary where the caller identity must be verified.
- **Fail-closed by default**: `client_auth_mandatory()` returns `true` — connections without valid client certificates are rejected with `UNAUTHENTICATED`. There is no "optional mTLS" mode.
- **CN/SAN validation**: The custom `AegisClientCertVerifier` checks both Common Name (`CN=agent-gateway`) and Subject Alternative Names (DNS/URI) against `RuntimeConfig.tls.expected_identity`. This provides defense-in-depth: even if a CA issues a cert with only CN, SAN must also match; and vice versa.
- **Configurable identity**: The expected identity is not hardcoded — it's `expected_identity` in `TlsConfig`, allowing different environments (dev/staging/prod) to use different caller identities.
- **Plain TLS was rejected**: Plain TLS would allow any client with a valid CA-signed cert to connect, including unauthorized agent-gateway instances or malicious actors who obtain a CA-signed cert.

### Tradeoffs
- Certificate management complexity: requires CA, server cert, client cert for every deployment
- mTLS handshake adds latency (~1-2ms) to every gRPC call
- Certificate rotation requires coordination between client and server
- The `AegisClientCertVerifier` parses X.509 certs manually using `x509-parser` — potential fragility if cert formats change

### Consequences
- Every `aegis-runtime` deployment requires CA cert, server cert/key, and client cert/key configured in `RuntimeConfig`
- The `grpc_boundary.rs` integration tests generate ephemeral certs via `rcgen` to test mTLS
- If TLS is not configured (`config.server.tls = None`), the server warns but still starts — this is a known gap (no enforcement that mTLS is required in production)

### Traceability
- Source: `src/grpc/tls.rs` `AegisClientCertVerifier`, `src/config/runtime.rs` `TlsConfig`, `src/grpc/server.rs` `build_tonic_tls_config`
- Spec: REQ-713, REQ-716