# Rust runtime: the global allocator on the IDF heap, the panic handler, and the
# sysroot libraries Rust crates link against. Every Rust component requires it.
idf_component_register(SRCS "/dev/null")
devkit_rust_crate(ROOT "src/lib.rs")

get_property(libdir GLOBAL PROPERTY DEVKIT_RUST_TARGET_LIBDIR)
# libgcc ahead of compiler_builtins so the intrinsics both define resolve to libgcc.
target_link_libraries(${COMPONENT_LIB} INTERFACE
    "${libdir}/liballoc.rlib" "${libdir}/libcore.rlib" gcc "${libdir}/libcompiler_builtins.rlib")
