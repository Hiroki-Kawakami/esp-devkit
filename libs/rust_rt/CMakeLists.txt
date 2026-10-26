# Rust runtime: the global allocator on the IDF heap, the panic handler, and the
# sysroot libraries Rust crates link against. Every Rust component requires it.
idf_component_register(SRCS "/dev/null")
devkit_rust_crate(ROOT "src/lib.rs")

get_property(libdir GLOBAL PROPERTY DEVKIT_RUST_TARGET_LIBDIR)
if(ESP_PLATFORM)
    # libgcc ahead of compiler_builtins so the intrinsics both define resolve to libgcc.
    target_link_libraries(${COMPONENT_LIB} INTERFACE
        "${libdir}/liballoc.rlib" "${libdir}/libcore.rlib" gcc "${libdir}/libcompiler_builtins.rlib")
    # Rust objects carry .note.GNU-stack and Xtensa GCC's do not, so ld would warn
    # that the latter imply an executable stack.
    target_link_options(${COMPONENT_LIB} INTERFACE "-Wl,-z,noexecstack")
else()
    # The toolchain's own host sysroot; the host libgcc/compiler-rt already
    # provides what compiler_builtins would.
    file(GLOB host_libs "${libdir}/liballoc-*.rlib" "${libdir}/libcore-*.rlib")
    list(SORT host_libs)
    target_link_libraries(${COMPONENT_LIB} ${host_libs})
endif()
