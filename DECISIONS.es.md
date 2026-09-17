# Decision Records — aegis

> 🇬🇧 [English version](./DECISIONS.md)
>
> Traducción fiel al español neutro de los 16 registros de decisiones de arquitectura (AD-001..AD-016). Se preservan los identificadores de código, rutas de archivos, identificadores de requisitos y términos técnicos en su forma original.

## AD-001: trap_on_grow_failure(true) — Comportamiento no especificado con cierre ante fallo

**Fecha**: 2026-09-06
**Fase**: 0
**Estado**: Aceptado

### Contexto
`StoreLimitsBuilder::trap_on_grow_failure(true)` configura Wasmtime para generar un `Trap` determinista cuando `memory.grow` supera el límite configurado, en lugar de devolver `-1` de forma controlada.

### Decisión
Usar `trap_on_grow_failure(true)` en la Fase 0 a pesar de ser un comportamiento no conforme con la especificación.

### Justificación
- **El cierre ante fallo es la prioridad**: la garantía central de la Fase 0 es "cualquier agotamiento de recursos = trap, nunca un error controlado".
- La especificación de Wasm permite que las implementaciones generen un trap ante OOM en `memory.grow`; devolver `-1` es una decisión de calidad de implementación.
- Validado en el spike: wasmtime 24.0 genera un trap con el mensaje "forcing trap when growing memory to N bytes" — determinista y correcto para nuestro modelo de seguridad.
- Revisar en la Fase 1+ cuando las concesiones de capacidades puedan requerir semánticas distintas para hosts específicos.

### Consecuencias
- Los módulos que superan el límite de memoria generan un trap de inmediato (sin oportunidad de manejar el OOM de forma controlada).
- Coherente con REQ-003 y REQ-007 (invariante transversal de cierre ante fallo).
- Documentado como desviación intencional del cumplimiento estricto de la especificación.

---

## AD-002: Interrupción de CPU solo por época — Sin metering de fuel

**Fecha**: 2026-09-06
**Fase**: 0
**Estado**: Aceptado

### Contexto
Wasmtime ofrece dos mecanismos para limitar el tiempo de CPU:
1. **Metering de fuel** (`consume_fuel(true)`): instrumenta cada instrucción con consumo de fuel.
2. **Interrupción por época** (`epoch_interruption(true)`): llamadas periódicas a `engine.increment_epoch()` desde un hilo temporizador.

### Decisión
Usar solo interrupción por época en la Fase 0; diferir el metering de fuel a la Fase 1+.

### Justificación
- **Simplicidad**: un único hilo temporizador frente a la instrumentación por instrucción.
- **Ajuste de alcance**: la Fase 0 demuestra que el aislamiento funciona; el fuel es observabilidad/métricas, no un límite de seguridad.
- **Limitación conocida**: la interrupción por época es imprecisa para bucles cerrados sin llamadas a funciones (Wasm solo verifica la época en los límites de llamada/bucle). Un `loop { br 0 }` sin llamadas puede no ceder durante todo el intervalo de época.
- Aceptable para la Fase 0 porque:
  - La prueba de integración hostil (REQ-006) usa una época de 10 ms y valida que se produce el trap.
  - Los módulos reales con llamadas host (Fase 1+) encontrarán las verificaciones de época en los límites de llamada.
  - El metering de fuel agrega overhead sin reforzar la propiedad de seguridad de la Fase 0.

### Consecuencias
- **Imprecisión documentada**: los bucles cerrados sin llamadas pueden superar el presupuesto de época antes de generar el trap.
- **Mitigación**: las funciones host de la Fase 1 proporcionarán puntos naturales de verificación de época.
- **Futuro**: se agregará el metering de fuel cuando los recibos necesiten informar el consumo de CPU (no como límite de seguridad).

---

## AD-003: EpochInterrupter — canal mpsc para la retransmisión del handle de hilo

**Fecha**: 2026-09-06
**Fase**: 0
**Estado**: Aceptado

### Contexto
El diseño original especificaba `EpochInterrupter` con una bandera de detención simple `Arc<AtomicBool>` más `JoinHandle`, usando `std::thread::current()` en `new()` para capturar el handle del hilo generado.

### Problema
`std::thread::current()` dentro de `EpochInterrupter::new()` devuelve el handle del hilo **del llamador**, no el del hilo generado. Esto causa:
- que `unpark()` en `Drop` despierte el hilo equivocado;
- que `join()` espere al hilo del llamador (deadlock o pánico).

### Decisión
Usar `std::sync::mpsc::channel` para retransmitir el handle `Thread` del propio hilo generado a su creador:
1. Generar el hilo.
2. El hilo generado llama a `std::thread::current()` y lo envía por el canal.
3. El creador recibe por `rx.recv()` y lo almacena en `EpochInterrupter.thread`.
4. `Drop` llama a `thread.unpark()` sobre el handle correcto.

### Implementación
```rust
pub fn new(engine: Engine, interval: Duration) -> Self {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let current = thread::current();  // Este ES el hilo generado
        let _ = tx.send(current);         // Envía su PROPIO handle de vuelta
        loop {
            thread::park_timeout(interval);
            if stop_clone.load(Ordering::Relaxed) { break; }
            engine.increment_epoch();
        }
    });

    let thread = rx.recv().expect("failed to receive thread handle");
    Self { thread, handle: Some(handle), stop }
}
```

### Consecuencias
- **Correctitud**: `unpark()` apunta al hilo temporizador real.
- **Costo de complejidad**: +canal +retransmisión de handle (~15 líneas).
- **Sin deadlock**: `rx.recv()` bloquea brevemente hasta que el hilo generado envía su handle.
- **Documentado aquí**: previene una futura "simplificación" que reintroduzca el deadlock.

---

## AD-004: Sandbox::new_with_config — Interruptor de interrupción por época para aislamiento de pruebas

**Fecha**: 2026-09-06
**Fase**: 0
**Estado**: Aceptado

### Contexto
El contador de época de Wasmtime es **global al proceso** — todas las instancias de `Engine` comparten la misma época monótonamente creciente. Esto significa que:
- La prueba A crea un sandbox con época de 10 ms → la época se incrementa rápidamente.
- La prueba B crea un sandbox con época de 10 s → la época ya es enorme por la prueba A → trap inmediato.

Esto vuelve la ejecución paralela de pruebas no determinista y la ejecución secuencial dependiente del orden.

### Decisión
Agregar `Sandbox::new_with_config(config: SandboxConfig, enable_epoch: bool) -> Result<Self>` como segundo constructor. Cuando `enable_epoch=false`, el `Engine` se crea sin `epoch_interruption(true)` y no se genera ningún `EpochInterrupter`. `Sandbox::new_with_limits(config)` sigue siendo la API pública para producción (siempre habilita la época).

### Justificación
- **El aislamiento de pruebas es obligatorio**: no se puede depender del orden de pruebas ni de `--test-threads=1` para la correctitud.
- **La superficie de producción no cambia**: `new_with_limits` sigue aplicando la interrupción por época.
- **Adición mínima de API**: un método extra, un parámetro booleano, sin tipos nuevos.
- **La seguridad no cambia**: `enable_epoch=false` solo se usa en pruebas; el código de producción no puede deshabilitar la época por accidente porque no es el constructor por defecto.

### Consecuencias
- **La API pública crece en 1 método** — documentado en DECISIONS.md para evitar una futura "simplificación" a un único constructor.
- **Capacidad solo de pruebas**: `enable_epoch=false` se usa únicamente en `tests/sandbox.rs` para la prueba de límite de memoria.
- **Documentado aquí**: previene la interpretación errónea como un "interruptor de desactivación de seguridad" — es estrictamente para aislamiento de pruebas contra el contador de época global de Wasmtime.

---

## Trazabilidad

| Decisión | Req de Spec | Sección de Diseño | Implementación |
|----------|-------------|-------------------|----------------|
| AD-001 | REQ-003, REQ-007 | Decisión: Store Limits | `StoreLimitsBuilder::trap_on_grow_failure(true)` |
| AD-002 | REQ-001, REQ-006 | Decisión: Engine Config | `Config::epoch_interruption(true)`, sin fuel |
| AD-003 | REQ-004 | Decisión: Epoch Timer | `EpochInterrupter::new()` con canal mpsc |
| AD-004 | REQ-003, REQ-004 | Aislamiento de pruebas | `Sandbox::new_with_config(config, enable_epoch: bool)` |
| AD-005 | REQ-510 | Brecha S-416-W-after-rename | `aegis_fs_write` trap + registro de auditoría tras rename |
| AD-006 | REQ-503, REQ-512 | Symlink traversal + corrección de byte NUL | `aegis_fs_write` verificación symlink `..` + recorte de NUL |
| AD-007 | REQ-701, REQ-702, REQ-704, REQ-714, REQ-715 | Pruebas de fallo en el límite gRPC (deuda explícita) | Documentado en AD-007; se requiere PR de endurecimiento posterior |

---
## AD-005: S-416-W-after-rename — Brecha de recibo en escritura tras rename exitoso

**Fecha**: 2026-09-08
**Fase**: 3 (enforcement)
**Estado**: Aceptado

### Contexto
`aegis_fs_write` usa escritura atómica (archivo temporal + `std::fs::rename`). La secuencia crítica:
1. Validar ruta, tamaño y memoria del guest.
2. Escribir los datos en un archivo temporal (`.aegis_tmp` en `allowed_root`).
3. `std::fs::rename(temp, target)` — el archivo ya es visible en la ruta destino.
4. Emitir el recibo de éxito mediante `ReceiptEmitter`.

Si el paso 4 (emit) falla después de que el paso 3 (rename) haya tenido éxito, existe una **asimetría fundamental** frente a `aegis_fs_read`:
- En `read`, cerrar ante fallo = "no se devuelven datos al guest" (memoria del guest completamente controlada).
- En `write`, el efecto secundario del sistema de archivos del host ya está cometido y es visible para otros procesos — no se puede retractar.

### Decisión
Si `emit()` falla después de un rename exitoso:
1. Registrar un registro de auditoría explícito de "FAIL-CLOSED VIOLATION" con el contexto completo (ruta, tamaño, error).
2. Devolver `Trap` a nivel de API (mantener el contrato de API de cierre ante fallo).
3. **NO intentar eliminar/revertir el archivo** — ya está cometido en el sistema de archivos del host.

Esto crea una **brecha en la cadena de recibos**: ocurrió una escritura real en el host, pero no se agregó ningún recibo firmado a la cadena.

### Justificación
- **No se puede deshacer una escritura en el sistema de archivos del host**: no existe un "des-escritura" atómico y portable después de que `rename` tenga éxito.
- **Bloquear la escritura por completo** requeriría una confirmación en dos fases (recibo pendiente → confirmación) o un coordinador de transacciones externo — fuera del alcance de la Fase 3.
- **Devolver éxito sin recibo** violaría en silencio la garantía de integridad de la cadena de recibos (Decisión 4).
- **Trap + registro de auditoría explícito** mantiene el cierre ante fallo a nivel de API siendo honesto sobre el efecto secundario.

### Mitigación parcial: el reintento idempotente es seguro
REQ-506 especifica semántica de sobrescritura (equivalente a `std::fs::write`). Si un llamador recibe `Trap` tras S-416-W-after-rename y reintenta:
- Misma ruta, mismos datos → el archivo se sobrescribe con contenido idéntico.
- Sin duplicación, sin corrupción, sin divergencia de estado.
- El estado del sistema de archivos es convergente; solo la cadena de recibos tiene una brecha.
- Esto acota el radio de explosión **operativo** pero NO cierra la brecha **de auditoría**.

### Consecuencias
- **Integridad de la cadena de recibos**: existe una brecha verificable para cualquier escritura en la que emit falle después del rename.
- **Rastro de auditoría**: la entrada explícita de registro "FAIL-CLOSED VIOLATION" proporciona trazabilidad manual.
- **Modelo de seguridad**: la capacidad de escritura tiene una garantía de auditoría más débil que la capacidad de lectura.
- **Corrección futura**: emisión de recibos en dos fases (pendiente antes del rename, confirmación después) o un KMS externo con atomicidad para cerrar esta brecha por completo.
- **Aplicabilidad**: esta brecha se aplica a CUALQUIER capacidad con efectos secundarios persistentes en el host (futuro `network.http` POST, etc.).

### Trazabilidad
- Diseño: `openspec/changes/phase3-enforcement/design.md` §6, §7.3
- Spec: `openspec/changes/phase3-enforcement/specs/filesystem-write/spec.md` REQ-510, S-416-W-after-rename
- Pruebas: `receipt_s416_write_after_rename` valida trap + registro + archivo escrito

---
## AD-006: Bypass de symlink traversal + manejo de byte NUL en la lectura de rutas

**Fecha**: 2026-09-09
**Fase**: 3 (enforcement)
**Estado**: Aceptado

### Contexto
Se descubrieron dos problemas de seguridad relacionados en `aegis_fs_write` después de la implementación inicial:

1. **Bypass de symlink traversal**: cuando un symlink apunta a un destino inexistente que contiene `..` (por ejemplo, `../../outside/evil.txt`), la rama `read_link` resolvía el destino contra el directorio padre pero NO normalizaba la ruta resultante antes de la verificación `starts_with` contra la raíz. Como `Path::starts_with` compara componentes literalmente (no normalizados), una ruta como `/tmp/abc123/../../outside/evil.txt` superaba la verificación `starts_with` contra `/tmp/abc123` (los primeros componentes coinciden, luego `..` frente al final del prefijo), permitiendo una fuga por escritura.

2. **Byte NUL en la lectura de rutas**: las rutas de la memoria del guest incluyen un terminador NUL (por ejemplo, `evil_symlink.txt\0`). `std::str::from_utf8` preserva el byte NUL, lo que hace que las operaciones del sistema de archivos fallen con "file name contained an unexpected NUL byte" en lugar de la verificación de seguridad prevista.

### Decisión
1. **Rechazo de `..` en el destino del symlink**: en la rama `read_link`, verificar explícitamente si el destino del symlink contiene componentes `..` y rechazarlo de inmediato con un trap (coherente con el rechazo de traversal REQ-503 para rutas del guest).

2. **Verificación de normalización de ruta como defensa en profundidad**: antes de la verificación `starts_with` contra la raíz, verificar que `canonical_path` no contenga ningún `ParentDir` (`..`). Esto detecta cualquier construcción de ruta que deje `..` sin normalizar.

3. **Recorte de bytes NUL**: recortar los bytes NUL finales de las rutas de la memoria del guest después de la decodificación UTF-8, tanto en `aegis_fs_read` como en `aegis_fs_write`.

### Justificación
- **Coherencia con REQ-503**: los destinos de symlink con `..` son intentos de traversal independientemente de si el destino existe aún.
- **Defensa en profundidad**: la verificación `starts_with` no sustituye a una normalización adecuada de la ruta. La verificación explícita de componentes `..` detecta cualquier ruta que se escape.
- **Cierre ante fallo coherente**: el manejo del byte NUL no debe cambiar los resultados de seguridad — es una corrección de higiene de parsing.

### Consecuencias
- **Nueva prueba**: `symlink_write_relative_traversal_traps` ejercita explícitamente el vector de ataque de symlink con destino relativo con `..` que no existe.
- **La corrección del byte NUL se aplica a lectura y escritura**: `aegis_fs_read` también tenía el mismo bug latente (las rutas del guest con terminadores NUL fallaban en las operaciones del sistema de archivos).
- **Las 81 pruebas pasan**, incluida la nueva prueba de symlink traversal.

### Trazabilidad
- Spec: `openspec/changes/phase3-enforcement/specs/filesystem-write/spec.md` REQ-503, REQ-512
- Diseño: `openspec/changes/phase3-enforcement/design.md` §5.2, §7
- Pruebas: `symlink_write_relative_traversal_traps`
- Commit: d2667fe
---
## AD-007: Pruebas de fallo en el límite gRPC — Deuda técnica explícita

**Fecha**: 2026-09-09
**Fase**: 5 (integración agent-gateway)
**Estado**: Aceptado (Deuda Documentada)

### Contexto
La Fase 5 implementa el servidor gRPC `aegis-runtime` que expone los RPC `Execute`, `VerifyChain` y `GetReceiptChain` sobre mTLS. La implementación está completa y verificada (97 pruebas pasan, clippy limpio). Sin embargo, la fase de verificación identificó **4 pruebas de rutas de fallo en el límite gRPC que aún no están implementadas**:

| # | Escenario | Spec | Riesgo |
|---|-----------|------|--------|
| 1 | S-701: trap por violación de Execute vía gRPC (path traversal, exceso de tamaño) | grpc-runtime-server | Alto — prueba el mapeo de error gRPC `FAILED_PRECONDITION` |
| 2 | S-702: fallo de firma de Execute vía gRPC | grpc-runtime-server | Alto — prueba el mapeo de error gRPC `INTERNAL` |
| 3 | S-704: recibo manipulado de VerifyChain vía gRPC | grpc-runtime-server | Alto — prueba el mapeo de error gRPC para payloads manipulados |
| 4 | S-721: fallo de firma de Execute con clave corrupta vía gRPC | signed-receipts | Alto — prueba el mapeo de error gRPC con clave corrupta |

Estos escenarios están **probados a nivel de unidad** (traps por violación del sandbox, fallos de firma de recibos, manipulación de verificación de cadenas) pero **no en el límite gRPC** — la nueva capa de protocolo introducida en la Fase 5.

### Decisión
Documentar estas 4 pruebas como **deuda técnica explícita** (AD-007) en lugar de implementarlas en la Fase 5. Crear un PR de endurecimiento posterior para implementarlas.

### Justificación
- **Las pruebas unitarias ya cubren la lógica subyacente**: los traps por violación del sandbox, los fallos de firma de recibos, la manipulación de verificación de cadenas y el manejo de claves corruptas están verificados a nivel de unidad/integración en `tests/sandbox.rs` y `tests/receipts.rs`.
- **La brecha es la capa de traducción del límite gRPC**: las pruebas faltantes verifican que los errores internos se mapeen correctamente a códigos de estado gRPC (`FAILED_PRECONDITION`, `INTERNAL`, `UNAUTHENTICATED`/`INVALID_CERT`) y con mensajes de error adecuados — esta es la nueva superficie que expone la Fase 5.
- **Implementar estas pruebas requiere infraestructura de pruebas gRPC significativa**: una configuración de pruebas adecuada requiere generar certificados mTLS válidos/inválidos, crear claves corruptas, manipular recibos protobuf y verificar códigos de estado gRPC exactos — un trabajo de infraestructura sustancial más allá del alcance de la Fase 5.
- **La documentación explícita de deuda se alinea con la práctica del proyecto**: AD-005 y AD-006 documentan brechas similares con honestidad en lugar de fingir completitud.

### Consecuencias
- **Brecha en la verificación a nivel gRPC**: las 4 rutas de fallo no se ejercitan de extremo a extremo a través del límite gRPC.
- **Mitigada por la cobertura unitaria**: la lógica subyacente es sólida; solo la capa de traducción del protocolo no está verificada.
- **Se requiere un PR de endurecimiento posterior**: un PR dedicado para implementar estas 4 pruebas de integración gRPC.
- **Sin riesgo de regresión funcional**: la lógica subyacente es sólida; solo la traducción del límite del protocolo no está verificada.

### Seguimiento
Crear un PR de endurecimiento (`hardening/grpc-boundary-tests`) con las siguientes tareas:
1. Agregar la prueba `execute_rpc_violation_trap_via_grpc` (path traversal, exceso de tamaño)
2. Agregar la prueba `execute_rpc_signing_failure_via_grpc` (clave corrupta, fallo de firma)
3. Agregar la prueba `verify_chain_tampered_receipt_via_grpc`
4. Agregar la prueba `execute_rpc_signing_failure_corrupt_key_via_grpc`

**RESTRICCIÓN OBLIGATORIA**: este PR de endurecimiento es la **tarea inmediatamente siguiente** al cierre de la Fase 5. NO compite con la Fase 6+ (`network.http`, ejecución WASM, etc.) por prioridad. Es el siguiente entregable explícito. Cualquier intento de iniciar el trabajo de la Fase 6 antes de que este PR esté fusionado constituye una violación de proceso. Propietario: ez (asignado). Objetivo: siguiente PR tras el cierre de la Fase 5.
- Informe de verificación: sección Warnings de `verify-report.md` (elementos 1-4)
- Specs: `openspec/changes/phase5-agent-gateway-integration/specs/grpc-runtime-server/spec.md` REQ-701, REQ-702, REQ-714, REQ-715
- Diseño: `openspec/changes/phase5-agent-gateway-integration/design.md` Estrategia de pruebas §3.4
- Fase: 5 (integración agent-gateway)
- **Tarea obligatoria siguiente**: PR `hardening/grpc-boundary-tests` (bloquea la Fase 6+)

---
## AD-008: Stub del RPC Execute — Cierre de la Fase 5 invalidado

**Fecha**: 2026-09-11
**Fase**: 5 (integración agent-gateway)
**Estado**: Aceptado (Invalidación Retroactiva)

### Contexto
La Fase 5 fue declarada **CERRADA** dos veces con una verificación PASS que reportaba "36/36 requisitos, 28/28 escenarios PASS" — incluidos REQ-714 ("El RPC Execute DEBE crear un sandbox por solicitud a partir de la configuración proporcionada y ejecutar la capacidad") y REQ-715 ("El RPC Execute DEBE generar un trap y devolver success=false ante cualquier violación").

**La investigación posterior reveló que el handler del RPC Execute nunca carga ni ejecuta un módulo WASM.** La implementación en `src/grpc/handlers/mod.rs`:

1. Crea un sandbox ✅
2. Parsea la configuración y valida la concesión de capacidad ✅
3. Establece el `ReceiptEmitter` compartido en el sandbox ✅
4. **NO llama a `instantiate_with_capabilities(wasm_bytes, capabilities)`** ❌
5. **NO invoca ninguna función exportada de un módulo instanciado** ❌
6. Devuelve una cadena de éxito fija mediante el stub `execute_filesystem_read` ❌

El stub (líneas 223-242) lo documenta explícitamente:
```rust
// For now, we return a simple success indicator
// In a full implementation, this would load a WASM module and execute it
// with the filesystem.read capability registered via Linker
let result = format!("filesystem.read executed for capability: {}", cap.capability_name());
```

### Decisión
**El cierre de la Fase 5 se invalida retroactivamente.** El PASS de verificación no reflejaba la realidad — ningún paso de verificación ejercitó la ejecución real de capacidades mediante wasmtime. La Fase 5 permanece **ABIERTA** hasta que:

1. El RPC Execute cargue un módulo WASM (fuente TBD: embebida, especificada en configuración o subida).
2. El RPC Execute llame a `sandbox.instantiate_with_capabilities(wasm_bytes, capabilities)`.
3. El RPC Execute invoque el punto de entrada exportado del módulo (por ejemplo, `run`, `execute`).
4. El RPC Execute capture el resultado real (no una cadena fija) y emita un recibo basado en la ejecución real.
5. Todos los escenarios S-700..S-715 se verifiquen contra la ejecución real (no contra el stub).

### Consecuencias
- **El PR `hardening/grpc-boundary-tests` está BLOQUEADO** — sus pruebas S-701, S-702, S-721 son `@ignore` precisamente porque Execute no ejecuta. No pueden pasar hasta que esto se corrija.
- **Todas las afirmaciones de "CERRADA" de la Fase 5 quedan retractadas** — sin archivo, sin entrega, sin inicio de la Fase 6.
- **La fiabilidad del proceso de verificación queda en cuestión** — esta es la segunda invalidación retroactiva en la Fase 5 (build declarado cerrado mientras estaba roto; verify PASS sin ejecución real). Las verificaciones futuras deben incluir una comprobación manual puntual de las rutas críticas.

### Causa raíz
La verificación se basó en la existencia de artefactos y en el éxito de compilación, no en la validación conductual del bucle central del runtime (carga WASM → instanciación → ejecución → recibo). El stub `execute_filesystem_read` se escribió con un comentario TODO pero nunca se marcó como bloqueante de la verificación.

### Trazabilidad
| Requisito de Spec | Estado | Evidencia |
|-------------------|--------|-----------|
| REQ-714 (Execute ejecuta la capacidad) | **PARCIAL** | WASM se carga/instancia/ejecuta vía `instantiate_with_capabilities` + export `execute`; captura de resultado TODO |
| REQ-715 (Execute genera trap ante violación) | **CUMPLIDO** | Path traversal y exceso de tamaño → gRPC success=false con error (S-701 PASS) |
| S-700 (Execute happy path) | **NO COMPROBABLE** | Requiere ejecución WASM real con captura de resultado |
| S-701 (Execute trap por violación) | **PASS** | Traversal y exceso de tamaño se mapean correctamente a gRPC success=false con mensaje de error |
| S-702 (Execute fallo de firma) | **PASS** | Fallo de firma forzado vía emitter solo de pruebas → gRPC success=false con error "receipt signing failure" |
| S-721 (Execute clave corrupta) | **NO APLICA** | Ed25519 valida en tiempo de carga; una clave que "carga pero falla al firmar" no puede existir. Cobertura en S-806/S-807 (validación de arranque). |

### Progreso (2026-09-11)
| Escenario | Estado | Notas |
|-----------|--------|-------|
| S-701 | ✅ PASS | Path traversal y exceso de tamaño vía ejecución WASM; FAILED_PRECONDITION → gRPC success=false |
| S-702 | ✅ PASS | `force_signing_failure()` en el emitter compartido → gRPC success=false con "receipt signing failure" |
| S-704 | ✅ PASS | VerifyChain con recibo manipulado → gRPC valid=false |
| S-721 | 📝 DOCUMENTADO | No aplica — Ed25519 valida en tiempo de carga; ver S-806/S-807 |

El RPC Execute ahora:
- Carga WASM desde `ExecuteRequest.wasm_module` (nuevo campo protobuf 3)
- Llama a `instantiate_with_capabilities(wasm_bytes, capabilities)`
- Invoca la función exportada `execute()` vía wasmtime
- Devuelve `success=false` con un error descriptivo ante violaciones (traversal, exceso de tamaño, fallo de firma)
- La captura del resultado de la memoria del guest sigue siendo TODO (devuelve un placeholder)

Restante para el cierre de AD-008:
1. Implementar la recuperación del resultado de la memoria del guest (usa `result_ptr` del export execute)
2. Re-ejecutar la verificación completa de la Fase 5 con ejecución WASM real
3. Archivar la Fase 5

---
## AD-009: Captura de resultado del RPC Execute — Brecha de integridad del recibo en el happy path

**Fecha**: 2026-09-11
**Fase**: 5 (integración agent-gateway)
**Estado**: Aceptado (Abierto)

### Contexto
El RPC Execute ahora carga, instancia y ejecuta módulos WASM vía wasmtime (AD-008). Sin embargo, la **captura de resultado del happy path no está implementada**:

| Componente | Comportamiento actual | Esperado |
|------------|----------------------|----------|
| Retorno de `execute_wasm_capability` | Placeholder `Ok(b"WASM execution completed")` | Contenido real de la memoria del guest en `result_ptr` |
| Parámetro `result` de `ReceiptEmitter.emit()` | `"success"` (fijo) | Resultado real de la ejecución del guest |
| Parámetro `path` de `ReceiptEmitter.emit()` | `""` (vacío) | Ruta/argumentos de la ejecución del guest |
| Parámetro `size` de `ReceiptEmitter.emit()` | `30` (longitud del placeholder) | Longitud real de los bytes del resultado |
| Campo `ExecuteResponse.result` | `"WASM execution completed"` | Bytes reales del resultado del guest |

El recibo **registra datos fabricados** (`result="success"`, `path=""`, `size=30`) en lugar de la salida real de la ejecución WASM del guest. Esto rompe la garantía central: *"prueba criptográfica de qué se ejecutó"*.

### Causa raíz
La función `execute_wasm_capability`:
1. Llama a la función exportada `execute` que devuelve `result_ptr: i32` (puntero a la memoria del guest).
2. Obtiene el export de memoria.
3. **No lee la memoria del guest en `result_ptr`** — marcado como TODO.
4. Devuelve bytes placeholder fijos.

### Consecuencias
- **Brecha de integridad del recibo**: cada ejecución exitosa produce un recibo con `result="success"` fabricado, no la salida real del cómputo.
- **Imposibilidad de verificación**: `agent-gateway` no puede verificar qué computó realmente el WASM.
- **Rastro de auditoría corrupto**: la cadena de recibos muestra ejecuciones exitosas pero con datos de resultado sin significado.
- **Diferente de AD-008**: AD-008 corrigió el mapeo de la *ruta de error* (S-701/702/704/721); esta es la integridad del *happy path*.

### Decisión
**Esta es una brecha separada de AD-008** (que corrigió el mapeo de errores gRPC para S-701/702/704/721). AD-008 cerró las pruebas del *límite de errores*; esta es la *integridad de datos del happy path*.

Crear `AD-009` para darle seguimiento. El PR de endurecimiento `hardening/grpc-boundary-tests` **puede fusionarse** — su alcance eran los 4 escenarios de error (S-701/702/704/721), todos ahora PASS. Este AD registra el trabajo del happy path necesario antes del archivo de la Fase 5.

### Seguimiento (Nueva rama: `fix/execute-result-capture`) — **COMPLETADO 2026-09-11**
1. ✅ ABI definido guest/host: el guest devuelve la tupla `(ptr, len)` desde el export `execute` (Opción B)
2. ✅ En `execute_wasm_capability`: leer la memoria del guest en `ptr` con longitud `len`, validar límites
3. ✅ Pasar el hash real del resultado a `emit(capability, "execute", hash, path, actual_len)`
4. ✅ Devolver los bytes reales del resultado en `ExecuteResponse.result`
5. ✅ Fixtures WAT actualizados: safe_read_module devuelve `(ptr, len)` vía local; traversal/size_exceed usan `unreachable`
6. ✅ Prueba: las 87 pruebas pasan, incluidas S-701/S-702/S-704

### Trazabilidad
| Requisito de Spec | Estado | Evidencia |
|-------------------|--------|-----------|
| REQ-701 (Execute ejecuta la capacidad) | **CUMPLIDO** | WASM se ejecuta, resultado capturado |
| REQ-708 (ExecuteResponse contiene el resultado) | **CUMPLIDO** | Devuelve los bytes reales de salida del guest |
| S-700 (Execute happy path) | **CUMPLIDO** | El recibo tiene hash, ruta y tamaño reales |

### Pruebas agregadas (2026-09-11)
| Prueba | Verifica |
|--------|----------|
| `execute_rpc_happy_path_result_capture` | ExecuteResponse.result = bytes del guest; resultado del recibo = hash BLAKE3; ruta = config; size = bytes |
| `get_receipt_chain_returns_chain` | La cadena tiene 4 recibos (2 read + 2 execute) después de 2 llamadas Execute |
| `verify_chain_valid_via_grpc` | La cadena de GetReceiptChain pasa VerifyChain con valid=true |

**ESTADO AD-009: RESUELTO** — Brecha de integridad del recibo en el happy path cerrada. Las 90 pruebas pasan. Listo para la verificación y el archivo de la Fase 5.

---

## AD-010: Funciones host vs WASI

**Fecha**: 2026-09-11
**Fase**: 6 (documentación)
**Estado**: Aceptado

### Contexto
Wasmtime soporta WASI (WebAssembly System Interface) como un conjunto de capacidades estandarizado. El proyecto aegis eligió implementar funciones host personalizadas (`aegis_fs_read`, `aegis_fs_write`) en lugar de usar WASI preview1 o preview2.

### Decisión
Usar funciones host personalizadas vía `Linker::func_wrap` en lugar de WASI.

### Justificación
- **Control de capacidades de grano fino**: WASI proporciona acceso amplio al sistema (fd_read, fd_write, path_open, etc.) que no se puede acotar fácilmente a una capacidad única como `filesystem.read` con un `allowed_root`. Las funciones host personalizadas permiten closures por capacidad que capturan `allowed_root` y `max_bytes` en tiempo de enlazado (link time).
- **Claridad del límite de seguridad**: cada función host tiene un nombre explícito (`aegis_fs_read`, `aegis_fs_write`) y se valida contra la configuración de capacidades antes de cualquier operación del sistema de archivos. El modelo plano basado en fd de WASI requeriría capas de autorización adicionales.
- **Integración de validación de rutas**: las funciones personalizadas incorporan la verificación completa de path traversal (rechazo de `..`, canonicalize + verificación `starts_with` contra la raíz, resolución de symlinks) directamente en el closure de la función host. WASI requeriría estas verificaciones en una capa de política separada.
- **Emisión de recibos**: las funciones personalizadas emiten recibos firmados en cada punto de validación (ruta fuera de límites, traversal, exceso de tamaño, éxito). WASI no tiene hooks para la emisión de recibos a nivel de syscall.
- **Extensibilidad futura**: las funciones host personalizadas pueden extenderse a `network.http`, `crypto.sign`, `crypto.verify` sin restricciones de compatibilidad de versiones de WASI.

### Compensaciones
- No estandarizado — el ABI personalizado significa que los módulos guest deben importar funciones del namespace `aegis`.
- Los módulos WASI del ecosistema no pueden ejecutarse sin una capa adaptadora.
- Más esfuerzo de implementación por capacidad frente al uso de imports WASI existentes.

### Consecuencias
- Los módulos WASM guest deben compilarse con imports `aegis`, no con imports WASI (el patrón `aegis_fs_read`/`aegis_fs_write`).
- Los módulos WASI producen errores de enlazado en la instanciación (S-4) — esto está documentado y es esperado.
- El enum `Capability` se mapea directamente al registro de funciones host, creando un acoplamiento estrecho entre las concesiones de capacidades y los exports de funciones host.

### Trazabilidad
- Diseño: `instantiate_with_capabilities()` en `src/sandbox/mod.rs`, `src/capabilities/mod.rs`
- Spec: Todas las specs de filesystem (REQ-101..107, REQ-501..512)

---

## AD-011: Feature flag test-utils para fallo de firma

**Fecha**: 2026-09-11
**Fase**: 6 (documentación)
**Estado**: Aceptado

### Contexto
El `ReceiptEmitter` necesita un mecanismo para forzar fallos de firma en pruebas y así verificar el comportamiento de cierre ante fallo (S-416, S-702). Esto requiere un método solo de prueba `force_signing_failure()` que no esté disponible en builds de producción.

### Decisión
Usar un feature flag de Cargo `test-utils` para restringir `force_signing_failure()` en `ReceiptEmitter`.

### Justificación
- **Aislamiento en tiempo de compilación**: el atributo `#[cfg(feature = "test-utils")]` garantiza que el campo y el método `force_signing_failure` estén completamente ausentes de los builds de release. No hay overhead de runtime ni superficie de ataque.
- **Patrón de dev-dependency**: el feature se habilita solo para `aegis = { path = ".", features = ["test-utils"] }` en `[dev-dependencies]`. Los usuarios de producción que dependen de `aegis` sin este feature obtienen un `ReceiptEmitter` limpio sin hooks de prueba.
- **Intención explícita**: el nombre del feature `test-utils` señala que todo lo que esté detrás de él es infraestructura de pruebas, no API de producción. Esto previene el uso accidental en código de producción.
- **Alternativa considerada**: la variable de entorno `AEGIS_TEST_MODE` (usada en `src/grpc/handlers/mod.rs` para la interrupción por época) se consideró pero se rechazó para `ReceiptEmitter` porque las variables de entorno son verificaciones en runtime, no garantías en tiempo de compilación. El feature flag proporciona un aislamiento más fuerte.

### Compensaciones
- Requiere `features = ["test-utils"]` en la dev-dependency, agregando un pequeño costo cognitivo para nuevos colaboradores.
- Dos rutas de código (restringidas por cfg) incrementan la superficie del `ReceiptEmitter`.
- El patrón `#[cfg(feature = "test-utils")]` debe aplicarse consistentemente — restricciones omitidas podrían filtrar hooks de prueba a producción.

### Consecuencias
- `cargo test` habilita automáticamente `test-utils` vía dev-dependency.
- `cargo build --release` no incluye hooks de prueba.
- `force_signing_failure()` es el único método solo de prueba en `ReceiptEmitter`; todos los demás métodos están listos para producción.

### Trazabilidad
- Fuente: `[features] test-utils = []` en `Cargo.toml`, bloques `#[cfg(feature = "test-utils")]` en `src/receipts/mod.rs`
- Pruebas: `receipt_emitter_signing_failure_forced`, `execute_rpc_signing_failure_via_grpc`

---

## AD-012: mTLS frente a TLS simple

**Fecha**: 2026-09-11
**Fase**: 6 (documentación)
**Estado**: Aceptado

### Contexto
El límite gRPC requiere TLS para la seguridad del transporte. El proyecto eligió TLS mutuo (mTLS) con validación del certificado de cliente frente a TLS simple (autenticación solo del servidor).

### Decisión
Exigir mTLS con validación CN/SAN del certificado de cliente (`AegisClientCertVerifier`) para todas las conexiones gRPC.

### Justificación
- **Autenticación mutua**: el TLS simple solo autentica al servidor ante el cliente. El mTLS autentica a ambas partes — el servidor presenta su certificado y el cliente debe presentar un certificado firmado por la CA configurada con CN/SAN que coincida con `expected_identity`. Esto es esencial para el límite agent-gateway (Go) ↔ aegis-runtime (Rust), donde debe verificarse la identidad del llamador.
- **Cierre ante fallo por defecto**: `client_auth_mandatory()` devuelve `true` — las conexiones sin certificados de cliente válidos se rechazan con `UNAUTHENTICATED`. No existe un modo "mTLS opcional".
- **Validación CN/SAN**: el `AegisClientCertVerifier` personalizado verifica tanto el Common Name (`CN=agent-gateway`) como los Subject Alternative Names (DNS/URI) contra `RuntimeConfig.tls.expected_identity`. Esto proporciona defensa en profundidad: incluso si una CA emite un certificado solo con CN, el SAN también debe coincidir; y viceversa.
- **Identidad configurable**: la identidad esperada no está fijada en el código — es `expected_identity` en `TlsConfig`, lo que permite que diferentes entornos (dev/staging/prod) usen identidades de llamador distintas.
- **El TLS simple fue rechazado**: el TLS simple permitiría que cualquier cliente con un certificado válido firmado por la CA se conectara, incluidos agent-gateway no autorizados o actores maliciosos que obtengan un certificado firmado por la CA.

### Compensaciones
- Complejidad de gestión de certificados: requiere CA, certificado de servidor y certificado de cliente para cada despliegue.
- El handshake mTLS agrega latencia (~1-2 ms) a cada llamada gRPC.
- La rotación de certificados requiere coordinación entre cliente y servidor.
- El `AegisClientCertVerifier` parsea certificados X.509 manualmente con `x509-parser` — posible fragilidad si cambian los formatos de certificados.

### Consecuencias
- Cada despliegue de `aegis-runtime` requiere cert de CA, cert/clave de servidor y cert/clave de cliente configurados en `RuntimeConfig`.
- Las pruebas de integración `grpc_boundary.rs` generan certificados efímeros vía `rcgen` para probar mTLS.
- Si TLS no está configurado (`config.server.tls = None`), el servidor advierte pero igual se inicia — esta es una brecha conocida (no se exige mTLS en producción).

### Trazabilidad
- Fuente: `AegisClientCertVerifier` + `build_tls_config` en `src/grpc/tls.rs`, `TlsConfig` en `src/config/runtime.rs`, acceptor TLS en `src/grpc/server.rs`
- Spec: REQ-713, REQ-716

---

## AD-013: Recortes de ampliación de alcance (Q6)

**Fecha**: 2026-09-11
**Fase**: 6 (documentación)
**Estado**: Aceptado

### Contexto
La Fase 6 se planificó originalmente para incluir varias características más allá de la documentación. A través del proceso SDD, estas se recortaron para mantener el enfoque y evitar introducir nuevos riesgos de implementación después del alerta cercana de la Fase 5 con PASS falsos.

### Decisión
La Fase 6 es solo de documentación. Todas las características de implementación se difieren a fases futuras.

### Elementos recortados del alcance de Q6
1. **Implementación de la capacidad `network.http`** — Requiere integración de cliente HTTP asíncrono, validación de URLs y rate limiting. Se difiere porque introduce nuevos modos de fallo (timeouts de red, resolución DNS) no cubiertos por los patrones de sandbox existentes.
2. **Capa de compatibilidad WASI** — Permitiría que los módulos WASI existentes se ejecuten en aegis. Requiere una implementación host de WASI o un adaptador. Se difiere porque entra en conflicto con el enfoque de funciones host personalizadas (AD-010) y diluiría el modelo de seguridad basado en capacidades.
3. **Metering de fuel** — Se difirió desde la Fase 0 (AD-002) como "Fase 1+". Sigue sin implementarse porque la interrupción solo por época es suficiente para los requisitos de seguridad actuales. El metering de fuel agrega overhead por instrucción y complejidad sin reforzar el límite de seguridad.
4. **Emisión de recibos en dos fases** — Propuesta como corrección de la brecha S-416-W-after-rename (AD-005). Requiere un protocolo de recibo pendiente/confirmación o un KMS externo con atomicidad. Se difiere porque el enfoque actual de registro de auditoría + trap con cierre ante fallo es adecuado para el modelo de amenazas.
5. **Capacidad GPU/computación** — No está en ninguna spec. Requeriría una función host de Wasmtime para compute shaders. Se difiere por completo — no existe spec.

### Justificación
- La Fase 5 tuvo 3 PASS falsos (ver Retrospectiva abajo) — introducir nueva implementación ahora arriesga repetir fallos de verificación.
- La fase de documentación debe consolidar las decisiones de arquitectura antes de agregar nuevas capacidades.
- Cada elemento diferido tiene una razón clara de diferimiento y puede explorarse individualmente en fases futuras.

### Consecuencias
- La Fase 6 produce solo artefactos de documentación (explore.md, ADRs, modelo de amenazas, retrospectiva).
- `network.http` sigue siendo una variante de capacidad en el enum `Capability` pero no tiene implementación de función host.
- El metering de fuel sigue diferido; la interrupción solo por época continúa como mecanismo de límite de CPU.
- La brecha de recibo S-416 sigue documentada pero sin corregir (mitigada por el registro de auditoría).

### Trazabilidad
- Relacionado: AD-002 (metering de fuel diferido), AD-005 (brecha de recibo S-416), AD-010 (funciones host vs WASI)

---

## AD-014: Arquitectura de la capacidad network.http (Fase 7)

**Fecha**: 2026-09-12
**Fase**: 7 (network.http)
**Estado**: Aceptado

### Contexto
`Capability::NetworkHttp` era un NO-OP nunca conectado a ninguna función host (elemento recortado #1 del ADR-013). La Fase 7 lo reinstaura como un cliente HTTPS solo-TLS, función host `aegis_http_fetch`, con allowlist de hosts, allowlist de métodos, rate limit por token-bucket por ejecución y tope de respuesta de 1 MiB. Las violaciones generan trap con cierre ante fallo y recibos de fetch firmados (S-601..S-606, REQ-601..REQ-610).

### Decisión
Adoptar las seis decisiones de diseño D1..D6 del diseño de la Fase 7 como la arquitectura de la capacidad de red.

| # | Decisión | Elección | Alternativas | Justificación |
|---|----------|----------|--------------|---------------|
| D1 | Cliente HTTP | `ureq = { version = "3", default-features = false, features = ["rustls"] }` (rustls 0.23, ring) | `reqwest::blocking` (pila hyper); ureq 2.12 | E/S bloqueante coincide con `Linker::func_wrap`; Rust puro; ring coincide con las dependencias existentes; sin gzip → los bytes del cuerpo son bytes de la red para BLAKE3 |
| D2 | Portador de política | `SandboxState.network_http: Option<NetworkHttpParams>` + `network_bucket`/`network_agent`/`network_fetch` | Extender `CapabilityConfig`; closures que capturan | `Option` es de costo cero para sandboxes sin red; el brazo `CapabilityConfig::from(NetworkHttp)` se conserva solo por exhaustividad del match |
| D3 | Rate limit | `TokenBucket` por ejecución en `SandboxState` (refill bajo demanda, sin `Mutex`/temporizador) | Bucket global (fuera de alcance); closure `Cell` (frágil al préstamo) | Un `Sandbox` nuevo por RPC Execute reinicia el bucket → semántica por ejecución (REQ-606, E-605) |
| D4 | Trap→gRPC | `msg.starts_with("network ")` como PRIMER guard en la cascada de traps del Execute; despacho por segunda palabra del catálogo (endpoint/method/connection/timeout/response-size/rate) | Envoltorio de código de error (excesivo); inserción posicional en la cascada (dependiente del orden) | El "size"/"exceed" de S-605 nunca debe llegar al brazo genérico de tamaño de fs; S-601 embebe una URL controlada por el guest (puede contener `..`/`size`/`max`) que nunca debe evaluarse contra los brazos de fs — solo un guard de prefijo es estructuralmente seguro |
| D5 | REQ-610 URL | El host almacena `FetchRecord { url, body_blake3, body_len }` en caso de éxito; el handler Execute lo lee para el recibo de ejecución (path = URL obtenida, result = BLAKE3(body), size = len del body) | El guest devuelve la URL vía resultado WASM (no confiable); allowlist unida por comas (rechazada en revisión) | El host es la única parte que sabe qué URL se obtuvo realmente |
| D6 | Momento del AD | El diseño registra la decisión; `AD-014` se transcribe en el apply | — | decision-log: los docs se actualizan en la misma unidad de trabajo que el código |

### Notas de implementación (refinamientos WU 2)
- `max_redirects(0)`: las redirecciones podrían escapar de la allowlist de hosts — endurecimiento con cierre ante fallo sobre el valor por defecto del diseño.
- `http_status_as_error(false)`: las respuestas 4xx/5xx devuelven sus cuerpos al guest en lugar de generar trap.
- Parsing de URLs vía `ureq::http::Uri` (sin agregar la dependencia `url`); cubre las necesidades de validación de scheme/authority/port/path.
- El buffer de salida demasiado pequeño ahora emite un recibo de trap (path=URL, size=body.len()) antes de abortar — cierra la brecha de auditoría clase AD-005 (fetch remoto observado sin recibo firmado).
- Reinstaura el elemento recortado #1 del ADR-013 (referencia de solo lectura): `openspec/changes/archive/2026-09-11-phase6-documentation/adrs/ADR-013-scope-creep-cuts-q6.md`.

### Notas de implementación (cierre de archivo, 2026-09-12)
- **Proveedor TLS solo-ring** (estado final del repo, commit `8b19898`): `rustls = { version = "0.23", default-features = false, features = ["ring","std","tls12","logging"] }` y `tokio-rustls = { default-features = false, features = ["ring","logging","tls12"] }` (Cargo.toml:39-42). aws-lc-rs queda eliminado del grafo de dependencias — `cargo tree -i aws-lc-rs` no encuentra nada (exit 101). Consecuencia: el feature `prefer-post-quantum` se descarta (ligado al feature de `aws_lc_rs`); la justificación "ring coincide con las dependencias existentes" de D1 queda implementada fielmente. Previamente se documentaba solo en un comentario de Cargo.toml (:35-38).
- **Implementación del recorrido de cadena D4** (`src/grpc/handlers/mod.rs:342-389`): wasmtime 24 envuelve los traps de funciones host en un frame de backtrace, por lo que el literal `msg.starts_with("network ")` nunca se dispara en el `Display` de nivel superior. El guard de red de primera prioridad recorre la cadena de errores vía `std::iter::successors(e.source(), ...)` y despacha sobre el primer frame cuyo mensaje comienza con `network ` (prefijos endpoint/method/connection/timeout/response size/rate limit), aislando estructuralmente todos los traps de red de los brazos de la cascada de fs.

### Consecuencias
- Los recibos a nivel Execute para fetches realizados reportan el `FetchRecord` registrado por el host (URL / BLAKE3(body) / longitud del body, REQ-610); los módulos que nunca hacen fetch caen en los valores genéricos.
- Los traps de red se manifiestan como gRPC `success=false` (FAILED_PRECONDITION) con mensajes mapeados limpios.
- Las configuraciones existentes siguen siendo válidas: `allowed_methods` tiene por defecto `["GET"]` vía serde default (REQ-601).
- El rollback está acotado: revertir el brazo `NetworkHttp` a NO-OP + el guard a la cascada de fs, eliminar este AD — no cambia el esquema de recibos.

### Trazabilidad
- Spec: `openspec/changes/archive/2026-09-12-phase7-network-http/specs/network-http/spec.md` REQ-601..REQ-610, S-601..S-606 (archivada 2026-09-12; promovida a `openspec/specs/network-http/spec.md`)
- Diseño: `openspec/changes/archive/2026-09-12-phase7-network-http/design.md` D1..D6, Flujo de datos
- Pruebas: `tests/network_http.rs` + `tests/grpc_boundary.rs` E2E REQ-610 (Fase 4)
- Relacionado: ADR-013 (recortes de alcance), AD-010 (funciones host vs WASI), AD-005 (clase de brecha de recibo)

---

## Trazabilidad

| Decisión | Req de Spec | Sección de Diseño | Implementación |
|----------|-------------|-------------------|----------------|
| AD-001 | REQ-003, REQ-007 | Decisión: Store Limits | `StoreLimitsBuilder::trap_on_grow_failure(true)` |
| AD-002 | REQ-001, REQ-006 | Decisión: Engine Config | `Config::epoch_interruption(true)`, sin fuel |
| AD-003 | REQ-004 | Decisión: Epoch Timer | `EpochInterrupter::new()` con canal mpsc |
| AD-004 | REQ-003, REQ-004 | Aislamiento de pruebas | `Sandbox::new_with_config(config, enable_epoch: bool)` |
| AD-005 | REQ-510 | Brecha S-416-W-after-rename | `aegis_fs_write` trap + registro de auditoría tras rename |
| AD-006 | REQ-503, REQ-512 | Symlink traversal + corrección de byte NUL | `aegis_fs_write` verificación symlink `..` + recorte de NUL |
| AD-007 | REQ-701, REQ-702, REQ-704, REQ-714, REQ-715 | Pruebas de fallo en el límite gRPC (deuda explícita) | Documentado en AD-007; se requiere PR de endurecimiento posterior |
| AD-008 | REQ-714, REQ-715 | Invalidación del stub del RPC Execute | Cierre de la Fase 5 invalidado retroactivamente |
| AD-009 | REQ-701, REQ-708, S-700 | Captura de resultado del RPC Execute | Lectura de memoria del guest en `result_ptr/len` |
| AD-010 | REQ-101..107, REQ-501..512 | Funciones host vs WASI | Funciones host personalizadas `Linker::func_wrap` |
| AD-011 | REQ-433, S-416, S-702 | Feature flag test-utils | `#[cfg(feature = "test-utils")]` en `ReceiptEmitter` |
| AD-012 | REQ-713, REQ-716 | mTLS frente a TLS simple | `AegisClientCertVerifier` con CN/SAN |
| AD-013 | Alcance de la Fase 6 | Recortes de ampliación de alcance | `network.http`, WASI, fuel, emisión en dos fases diferidos |
| AD-014 | REQ-601..REQ-610, S-601..S-606 | D1..D6 | Función host `aegis_http_fetch` + recibo de ejecución respaldado por `FetchRecord` |
| AD-015 | REQ-001, REQ-400, REQ-715, REQ-717, REQ-723 | §3, §4, §6, §7 | `consume_fuel(true)` + `set_fuel` + `fuel_consumed` en recibos Execute |
| AD-016 | REQ-755, clase AD-005 | §5, ADR-016 | Fallo de firma en Commit — brecha documentada con compensación externa |

---

## AD-015: Metering de fuel vía `consume_fuel` de Wasmtime

**Fecha**: 2026-09-14
**Fase**: 8 (fuel metering)
**Estado**: Aceptado

### Contexto
Los recibos Execute carecían de cualquier señal de uso de CPU; la interrupción por época acota el tiempo de reloj pero no puede reportarse de forma determinista por ejecución. La skill wasmtime-sandbox sugirió una API obsoleta (`add_fuel`, `fuel_consumed`) que no existe en wasmtime 24 (Explore #125, spike de diseño R1).

### Decisión
Habilitar `consume_fuel` de wasmtime en la creación del engine para cada sandbox (REQ-001), establecer un presupuesto explícito por ejecución vía `store.set_fuel()` — nunca confiando en el valor por defecto 0 — con un valor por defecto calibrado de 10 M de fuel (spike de diseño, Sección 6). Reportar `fuel_consumed` solo en los recibos Execute (D5), como último campo, skip-if-zero, byte-idéntico en 0 (E-802). Clasificar el agotamiento mediante un brazo D4 dedicado (literal determinista `all fuel consumed`) colocado después del guard de red y antes de la cascada de fs (D7). El fuel es contabilidad de instrucciones; la interrupción por época sigue siendo el límite de seguridad de tiempo de reloj (complementarios, no excluyentes — D1).

### Justificación
- **Contabilidad de CPU determinista por ejecución en los recibos** — los operadores y verificadores pueden ver el consumo exacto de fuel.
- **Clasificación de agotamiento S-802 estable** — el brazo de fuel detecta los traps `all fuel consumed` de forma determinista antes que la época.
- **Las configuraciones sin la perilla conservan el comportamiento actual** (E-804) — el valor por defecto de 10 M proporciona un margen de 909.090× sobre el execute máximo con forma E2E (11 fuel).
- **Los operadores pueden elevar `execution.fuel_budget` para guests con cómputo intensivo** (E-805) — perilla documentada.
- **Los recibos de capacidades llevan deliberadamente 0** (R4) — 23 de 25 sitios de emisión pasan 0; solo los 2 sitios Execute pasan fuel.
- **La lógica del verificador no se toca** (R6) — skip-if-zero + último campo hace que los recibos con fuel=0 sean byte-idénticos al esquema anterior al cambio.

### Consecuencias
- Cada sandbox creado para ejecución corre con `consume_fuel(true)` + `set_fuel(budget)` explícito — nunca dependiendo del valor por defecto 0 del store (REQ-001, E-801).
- El agotamiento de fuel se clasifica como `"fuel budget exceeded"` vía el brazo dedicado de la cascada D4 (S-802, E-803).
- El campo `fuel_consumed` en `ExecutionReceipt` es skip-if-zero, último campo, lo que permite verificación de cadenas retrocompatible (E-802).
- El presupuesto por defecto de 10 M de fuel calibró por spike: margen de 909.090× sobre el execute E2E máximo (11 fuel); ~770 K iteraciones aritméticas antes del agotamiento; trap determinista a ~7-14 ms (muy por debajo de la época de 100 ms).
- La perilla de operador `execution.fuel_budget: Option<u64>` se canaliza de config → servicio → sandbox (E-804, E-805).

### Trazabilidad
- Diseño: `openspec/changes/phase8-fuel-metering/design.md` §3, §4, §6, §7
- Specs: sandbox-init (REQ-001), receipt-verifier (REQ-400), signed-receipts (REQ-723), grpc-runtime-server (REQ-715, REQ-717)
- Pruebas: `tests/fuel.rs` (S-801, S-802, E-803, E-804, E-805), `src/sandbox/mod.rs` (E-801, E-801-inverse), `src/receipts/mod.rs` (E-802, S-803), `src/config/runtime.rs` (E-804, E-805)
- Precedente: AD-014/D6 de la Fase 7 (documentación en la misma unidad de trabajo que el código)

---

## AD-016: Fallo de firma en Commit — Brecha documentada con compensación externa

**Fecha**: 2026-09-15
**Fase**: 9 (recibos de dos fases)
**Estado**: Aceptado

### Contexto

Los recibos de dos fases (Fase 9) introducen un recibo `prepare` firmado antes de que el WASM se ejecute. El flujo:

1. `ExecutePrepare` → sandbox creado + recibo `prepare` firmado (`phase="prepare"`, `result="pending"`, `pending_hash = blake3(prev_hash || prepare_canonical)`) → `prepare_hash` devuelto al llamador
2. `ExecuteCommit` → el WASM se ejecuta en el sandbox preparado → resultado/fuel capturados → recibo `commit` firmado con `phase="commit"`, `pending_hash` copiado del prepare → la entrada `prepare` se elimina del mapa de pendientes

**La brecha**: si `ExecuteCommit` ejecuta el WASM (los efectos secundarios del host son reales y quedan cometidos — por ejemplo, `aegis_fs_write` realiza el `rename()` atómico) → **la firma final del `commit` falla** (disco lleno, corrupción de clave, sesgo de reloj, contención de locks, fallo forzado por prueba): los efectos secundarios son reales y quedan cometidos, pero no se produce ningún recibo `commit`.

Esta es la misma clase de mitigación que **AD-005** (S-416-W-after-rename): existe un recibo firmado *antes* del efecto secundario, pero el recibo final no llega a materializarse.

### Decisión

Misma clase de mitigación que AD-005:

1. El recibo `prepare` firmado (`phase="prepare"`) **existe en la cadena** — prueba la intención y el estado previo a la ejecución.
2. El `ReceiptEmitter` emite un recibo `abort` con `phase="abort"`, `pending_hash` = el `pending_hash` del prepare, `result="aborted"`, `error="commit_signing_failed"`, `fuel_consumed=0`.
3. La cadena de recibos muestra: `prepare` → `abort(commit_signing_failed)`.
4. **Compensación externa requerida**: entrada de registro de auditoría + alerta al operador (idéntico al patrón AD-005).
5. Esta es una **brecha conocida con mitigación documentada** — NO un fallo silencioso.

### Comportamiento del verificador

`ReceiptChain::verify_chain` acepta `prepare` → `abort(commit_signing_failed)` como cadena terminal válida. El `pending_hash` coincide, el `prev_hash` del abort es igual al `pending_hash` del prepare, y la máquina de estados limpia `expected_pending_hash` después del abort.

### Bootstrap del pending hash (WU3/WU4)

**Detalle de implementación crítico**: el `pending_hash` en el recibo `prepare` se calcula como `blake3(prev_hash || prepare_canonical_bytes_without_pending_hash)`. Como el campo `pending_hash` aparece en los bytes canónicos, existe una dependencia circular:

- `pending_hash` se deriva de los bytes canónicos del recibo prepare.
- Pero el recibo prepare *contiene* `pending_hash` como campo.

**Solución** (implementada en WU3/WU4):
1. Crear un recibo prepare temporal con `pending_hash = [0;32]` y un **timestamp fijo**.
2. Calcular `pending_hash = blake3(prev_hash || canonical_bytes_of_temp_receipt)`.
3. Crear el recibo prepare final con el `pending_hash` calculado y el **mismo timestamp fijo**, y volver a firmarlo.
4. El verificador usa `canonical_bytes_for_chain_without_pending_hash()` (que pone `pending_hash` en cero antes de hashear) para recalcular y verificar el mismo `pending_hash`.

El timestamp fijo garantiza que los bytes canónicos sean idénticos entre el pase de cálculo del hash y el pase final de firma. El verificador pone `pending_hash` en cero durante su recálculo para coincidir con el bootstrap original.

### Alternativas consideradas

- **Reintento automático de firma (3×)**: enmascara la causa raíz (corrupción de clave, disco lleno), agrega latencia, sin garantía.
- **Emitir recibo sin firmar + flag**: rompe los invariantes de verificación de la cadena (todos los recibos deben estar firmados).
- **Bloquear Commit hasta que la firma tenga éxito**: espera no acotada, sin garantía de progreso.

### Consecuencias

- Los operadores DEBEN monitorear los recibos `abort` con `error="commit_signing_failed"` y correlacionarlos con los efectos secundarios del host.
- La integración con el registro de auditoría (fuera del alcance de v1) automatizará esta correlación.
- El verificador acepta prepare→abort como cadena terminal válida (REQ-760).
- AD-016 se registra en DECISIONS.md en la misma unidad de trabajo que el código (precedente AD-014 D6).

### Trazabilidad

- Diseño: `openspec/changes/phase9-two-phase-receipts/design.md` §5, ADR-016
- Spec: `openspec/changes/phase9-two-phase-receipts/specs/grpc-runtime-server/spec.md` REQ-755
- Relacionado: AD-005 (brecha S-416-W-after-rename, misma clase de mitigación)
- Pruebas: `tests/two_phase.rs` — la prueba forzada `commit_signing_failed` valida que el prepare existe + que se emite abort con `error="commit_signing_failed"`

---
