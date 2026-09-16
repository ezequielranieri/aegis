# aegis

**aegis** es un runtime en Rust para **ejecución segura de WebAssembly** con seguridad basada en capacidades, recibos de ejecución firmados y un servidor gRPC con TLS mutuo.

> [English](./README.md)
>
> Documentación: [DECISIONS.es.md](./DECISIONS.es.md) — registros de decisiones de arquitectura y constitución del proyecto

> **Estado**: Fase 9 (recibos de dos fases) implementada — `cargo test` en verde (160 pruebas aprobadas), `cargo clippy --all-targets -- -D warnings` limpio, `cargo fmt --check` limpio.

## Qué resuelve

Ejecutar WebAssembly no confiable solo es seguro si cada acceso a recursos está restringido y cada efecto secundario es verificable. aegis aplica un modelo de seguridad con cierre ante fallo (fail-closed) a la ejecución de WASM:

- **Funciones host basadas en capacidades** en lugar de WASI: los guest reciben exactamente las capacidades declaradas en la política — `filesystem.read`, `filesystem.write`, `network.http` — y nada más. Cada capacidad otorga acceso acotado y validado (por ejemplo, un `allowed_root` para acceso a archivos o allowlists de hosts para HTTP).
- **Recibos de ejecución firmados y encadenados por hash**: cada ejecución produce un recibo firmado con Ed25519 (hash BLAKE3 encadenado) que prueba *qué se ejecutó, con qué resultado y a qué costo*. Los recibos son verificables offline con `aegis-verify`.
- **Ejecución de dos fases**: `ExecutePrepare` / `ExecuteCommit` / `ExecuteAbort` producen un recibo `prepare` firmado antes de ejecutar cualquier WASM y luego un recibo `commit` (o `abort`) firmado — de modo que tanto la intención como el resultado son verificables.
- **Límites de recursos estrictos**: sandbox Wasmtime con 1 MiB de memoria, 1024 elementos de tabla, 4 instancias y 2 memorias por store; interrupción por época como límite de CPU en tiempo de reloj y metering de fuel para contabilidad determinista de CPU por ejecución.
- **TLS mutuo en el límite gRPC**: el servidor exige certificados de cliente firmados por una CA configurada cuyo CN/SAN coincida con la identidad esperada — no existe un modo de TLS opcional.

## Arquitectura

```
┌──────────────┐     mTLS gRPC      ┌────────────────────────────────┐
│  agent-gateway │ ───────────────▶ │  aegis-runtime (Rust)            │
│  o cualquier   │                  │  ┌────────────────────────────┐  │
│  cliente       │                  │  │ Execute / Prepare / Commit │  │
└──────────────┘                    │  │ Verify / GetReceiptChain   │  │
                                    │  └────────────┬───────────────┘  │
                                    │               ▼                  │
                                    │  ┌────────────────────────────┐  │
                                    │  │ ReceiptEmitter (Ed25519 +   │  │
                                    │  │ BLAKE3, encadenado por hash)│  │
                                    │  └────────────┬───────────────┘  │
                                    │               ▼                  │
                                    │  ┌────────────────────────────┐  │
                                    │  │ Sandbox Wasmtime            │  │
                                    │  │  · funciones host de cap.   │  │
                                    │  │  · límites de recursos      │  │
                                    │  │  · época + metering de fuel │  │
                                    │  └────────────────────────────┘  │
                                    └────────────────────────────────┘
```

El runtime es declarativo: un archivo de configuración TOML declara el servidor, la identidad TLS, la clave de firma de recibos, los valores por defecto de la política y las perillas de ejecución (`max_concurrent`, `fuel_budget`). El control de acceso es *denegación por defecto*: un guest solo obtiene las capacidades otorgadas explícitamente en la configuración de su solicitud.

## Inicio rápido

Requisitos: Rust 1.98 o superior (verificado con 1.98.1), edition 2021.

```sh
# Compilar los binarios
cargo build

# Ejecutar la suite completa de pruebas (no se necesitan feature flags:
# test-utils se habilita automáticamente vía dev-dependencies, ver AD-011)
cargo test
```

Los dos binarios generados:

| Binario | Propósito | Uso |
|---------|-----------|-----|
| `aegis-runtime` | Servidor gRPC (mTLS, recibos, sandbox) | `aegis-runtime --config config/runtime.toml.example` |
| `aegis-verify` | Verificador offline de cadenas de recibos | `aegis-verify <chain.json> <public_key_b64>` |

## Configuración

La configuración del runtime es TOML (`RuntimeConfig`). Consulte [`config/runtime.toml.example`](./config/runtime.toml.example) para ver el ejemplo completo anotado:

```toml
[server]
host = "0.0.0.0"
port = 50051

[server.tls]              # mTLS: el CN/SAN del certificado de cliente debe coincidir con expected_identity
# ca_cert, server_cert, server_key, expected_identity ...

[receipts]
key_path = "/etc/aegis/keys/ed25519-private.pkcs8.pem"

[execution]
max_concurrent = 8        # ejecuciones en curso; al excederse → RESOURCE_EXHAUSTED
fuel_budget = 10_000_000  # fuel por ejecución; al agotarse → "fuel budget exceeded"
```

La `PolicyConfig` por solicitud (TOML) otorga capacidades al guest — por ejemplo, `filesystem.read` con un `allowed_root`, o `network.http` con allowlists de hosts y métodos.

## Estructura del repositorio

```
config/       runtime.toml.example (configuración de ejemplo anotada)
openspec/     specs, cambios archivados (documentación estilo openspec)
proto/        definiciones protobuf aegis/v1 (Execute, Prepare/Commit/Abort, Verify, Health)
src/          runtime: sandbox, receipts, capabilities, policy, grpc, observability
tests/        pruebas de integración: sandbox, config, fuel, network_http, two_phase, límite gRPC
DECISIONS.md  registros de decisiones de arquitectura (AD-001..AD-016)
```

## Documentación

- [DECISIONS.es.md](./DECISIONS.es.md) — 16 registros de decisiones de arquitectura: comportamiento del sandbox con cierre ante fallo (AD-001..AD-004), endurecimiento del sistema de archivos (AD-005, AD-006), límite gRPC (AD-007..AD-009), funciones host vs WASI (AD-010), features de aislamiento de pruebas (AD-011), mTLS (AD-012), recortes de alcance (AD-013), capacidad de red (AD-014), metering de fuel (AD-015), recibos de dos fases (AD-016).
- `openspec/specs/` — requisitos y escenarios de cada capacidad.

## Licencia

[MIT](./LICENSE)