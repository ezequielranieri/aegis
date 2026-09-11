;; Test WASM module for aegis gRPC boundary tests
;;
;; Imports:
;;   aegis.fs_read(path_ptr: i32, path_len: i32, out_ptr: i32, out_len: i32) -> i32
;;   aegis.fs_write(path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32) -> i32
;;
;; Exports:
;;   execute() -> i32 (returns 0 on success, non-zero on trap)
;;   memory

(module
  (import "aegis" "fs_read" (func $fs_read (param i32 i32 i32 i32) (result i32)))
  (import "aegis" "fs_write" (func $fs_write (param i32 i32 i32 i32) (result i32)))

  (memory 1)
  (export "memory" (memory 0))

  ;; Data section for test strings
  (data (i32.const 0) "safe_file.txt\00")
  (data (i32.const 16) "malicious_path\00")
  (data (i32.const 32) "../../../etc/passwd\00")
  (data (i32.const 48) "test content for write\00")
  (data (i32.const 72) "output_buffer\00")

  ;; execute function - calls fs_read with safe path
  (func $execute_safe_read (result i32)
    ;; Call fs_read with safe path at offset 0, length 13
    ;; Output buffer at offset 72, length 100
    local.get 0
    i32.const 0
    i32.const 13
    i32.const 72
    i32.const 100
    call $fs_read
    return
  )

  ;; execute function - calls fs_read with traversal path
  (func $execute_traversal_read (result i32)
    ;; Call fs_read with traversal path at offset 32, length 16
    local.get 0
    i32.const 32
    i32.const 16
    i32.const 72
    i32.const 100
    call $fs_read
    return
  )

  ;; execute function - calls fs_write with safe path
  (func $execute_safe_write (result i32)
    ;; Call fs_write with safe path at offset 16, length 14
    ;; Data at offset 48, length 21
    local.get 0
    i32.const 16
    i32.const 14
    i32.const 48
    i32.const 21
    call $fs_write
    return
  )

  ;; Default execute - calls safe read
  (func $execute (export "execute") (result i32)
    call $execute_safe_read
  )
)