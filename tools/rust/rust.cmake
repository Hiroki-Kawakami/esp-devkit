set(DEVKIT_RUST_DIR "${CMAKE_CURRENT_LIST_DIR}")
cmake_policy(SET CMP0116 NEW)

# Sets <triple_out> to the Rust target and <clang_out> to the matching libclang
# target flags for bindgen.
function(_devkit_rust_target triple_out clang_out)
    if(IDF_TARGET MATCHES "^esp32(s3)?$")
        set(triple xtensa-${IDF_TARGET}-none-elf)
        set(clang --target=xtensa-esp-elf -mcpu=${IDF_TARGET})
    elseif(IDF_TARGET MATCHES "^esp32(p4|s31)$")
        set(triple riscv32imafc-unknown-none-elf)
        set(clang --target=riscv32-esp-elf -march=rv32imafc_zicsr_zifencei -mabi=ilp32f)
    elseif(IDF_TARGET MATCHES "^esp32(c5|c6|c61|h2|h21)$")
        set(triple riscv32imac-unknown-none-elf)
        set(clang --target=riscv32-esp-elf -march=rv32imac_zicsr_zifencei -mabi=ilp32)
    else()
        message(FATAL_ERROR "devkit_rust_crate: no Rust target for ${IDF_TARGET}")
    endif()
    set(${triple_out} ${triple} PARENT_SCOPE)
    set(${clang_out} ${clang} PARENT_SCOPE)
endfunction()

# libclang finds neither the toolchain's libc headers nor IDF's build-wide
# definitions on its own; gcc gets the former through IDF's -specs.
function(_devkit_rust_clang_args out clang_target)
    get_filename_component(toolchain "${CMAKE_C_COMPILER}" DIRECTORY)
    get_filename_component(toolchain "${toolchain}" DIRECTORY)
    if(CONFIG_IDF_TARGET_ARCH_XTENSA)
        set(sysroot "${toolchain}/xtensa-esp-elf")
    else()
        set(sysroot "${toolchain}/riscv32-esp-elf")
    endif()
    set(sysincludes "")
    if(CONFIG_LIBC_PICOLIBC)
        list(APPEND sysincludes "${toolchain}/picolibc/include")
    endif()
    list(APPEND sysincludes "${sysroot}/include")

    set(args ${clang_target} -nostdlibinc)
    foreach(dir ${sysincludes})
        if(NOT IS_DIRECTORY "${dir}")
            message(FATAL_ERROR "devkit_rust_crate: libc headers not found at ${dir}")
        endif()
        list(APPEND args "-isystem${dir}")
    endforeach()
    idf_build_get_property(c_options C_COMPILE_OPTIONS)
    foreach(option ${c_options})
        if(option MATCHES "^-std=")
            list(APPEND args "${option}")
        endif()
    endforeach()
    idf_build_get_property(definitions COMPILE_DEFINITIONS)
    foreach(definition ${definitions})
        list(APPEND args "-D${definition}")
    endforeach()
    set(${out} ${args} PARENT_SCOPE)
endfunction()

# devkit_rust's DEVKIT_RUST_RMETA_<crate> properties are read through generator
# expressions, so a crate can name a dependency whose CMakeLists has not run yet.
function(_devkit_rust_init)
    get_property(done GLOBAL PROPERTY DEVKIT_RUST_INITIALIZED)
    if(done)
        return()
    endif()
    set_property(GLOBAL PROPERTY DEVKIT_RUST_INITIALIZED TRUE)

    find_program(rustc rustc REQUIRED NO_CACHE)
    set_property(GLOBAL PROPERTY DEVKIT_RUST_RUSTC "${rustc}")
    idf_build_get_property(build_dir BUILD_DIR)
    idf_build_get_property(python PYTHON)
    idf_build_get_property(sdkconfig_json SDKCONFIG_JSON)
    idf_build_get_property(sdkconfig_menus SDKCONFIG_JSON_MENUS)
    set(out "${build_dir}/rust")
    file(MAKE_DIRECTORY "${out}")

    _devkit_rust_target(triple clang_target)
    execute_process(
        COMMAND "${rustc}" --print target-libdir "--target=${triple}"
        OUTPUT_VARIABLE libdir
        OUTPUT_STRIP_TRAILING_WHITESPACE
        COMMAND_ERROR_IS_FATAL ANY)
    set_property(GLOBAL PROPERTY DEVKIT_RUST_TARGET_LIBDIR "${libdir}")

    if(CONFIG_COMPILER_OPTIMIZATION_NONE)
        set(opt 0)
    elseif(CONFIG_COMPILER_OPTIMIZATION_DEBUG)
        set(opt 1)
    elseif(CONFIG_COMPILER_OPTIMIZATION_SIZE)
        set(opt s)
    else()
        set(opt 2)
    endif()
    if(CONFIG_COMPILER_OPTIMIZATION_ASSERTIONS_DISABLE)
        set(debug_assertions off)
    else()
        set(debug_assertions on)
    endif()

    set(common
        "--target=${triple}"
        "-Cpanic=abort"
        "-Copt-level=${opt}"
        "-Cdebuginfo=2"
        "-Cdebug-assertions=${debug_assertions}"
        "-Zbinary-dep-depinfo"
        "--color=always"
        "-Ldependency=${out}")
    if(opt STREQUAL "0" OR opt STREQUAL "1")
        list(APPEND common "-Cincremental=${out}/incremental")
    endif()
    list(JOIN common "\n" common_content)
    file(CONFIGURE OUTPUT "${out}/common.args" CONTENT "${common_content}\n" @ONLY)

    find_program(bindgen bindgen REQUIRED NO_CACHE)
    set_property(GLOBAL PROPERTY DEVKIT_RUST_BINDGEN "${bindgen}")
    _devkit_rust_clang_args(clang_args "${clang_target}")
    list(JOIN clang_args "\n" clang_content)
    file(CONFIGURE OUTPUT "${out}/clang.args" CONTENT "${clang_content}\n" @ONLY)

    execute_process(
        COMMAND "${python}" "${DEVKIT_RUST_DIR}/sdkconfig_rs.py"
                "${sdkconfig_json}" "${sdkconfig_menus}"
                "${out}/sdkconfig.rs" "${out}/sdkconfig.args"
        RESULT_VARIABLE result)
    if(NOT result EQUAL 0)
        message(FATAL_ERROR "devkit_rust_crate: sdkconfig_rs.py failed")
    endif()

    add_custom_command(
        OUTPUT "${out}/libsdkconfig.rmeta"
        COMMAND "${rustc}" "@${out}/common.args" --edition=2024 --crate-type=rlib
                --crate-name=sdkconfig "--emit=metadata=${out}/libsdkconfig.rmeta"
                "${out}/sdkconfig.rs"
        DEPENDS "${out}/sdkconfig.rs" "${out}/common.args"
        COMMENT "rustc sdkconfig"
        VERBATIM)
    add_custom_target(devkit_rust_sdkconfig DEPENDS "${out}/libsdkconfig.rmeta")
    add_library(devkit_rust INTERFACE)
endfunction()

function(_devkit_rust_header_regexes out)
    set(regexes "")
    foreach(header ${ARGN})
        string(REPLACE "." "\\." header "${header}")
        string(REPLACE "+" "\\+" header "${header}")
        list(APPEND regexes "(.*/)?${header}")
    endforeach()
    set(${out} ${regexes} PARENT_SCOPE)
endfunction()

# Generates <gen>/bindings.rs for the headers, plus <gen>/static.c with the
# wrappers for their static inline functions, compiled into the component.
function(_devkit_rust_bindgen name out gen)
    cmake_parse_arguments(B "" "" "HEADERS;EXPORTS" ${ARGN})
    get_property(bindgen GLOBAL PROPERTY DEVKIT_RUST_BINDGEN)

    set(header_content "")
    foreach(header ${B_HEADERS} ${B_EXPORTS})
        string(APPEND header_content "#include \"${header}\"\n")
    endforeach()
    file(CONFIGURE OUTPUT "${gen}/bindgen.h" CONTENT "${header_content}" @ONLY)

    _devkit_rust_header_regexes(allow ${B_HEADERS} ${B_EXPORTS})
    set(options --use-core)
    foreach(regex ${allow})
        list(APPEND options "--allowlist-file=${regex}")
    endforeach()
    list(JOIN options "\n" options_content)
    file(CONFIGURE OUTPUT "${out}/${name}.bindgen.args" CONTENT "${options_content}\n" @ONLY)

    set(export_args "")
    if(B_EXPORTS)
        _devkit_rust_header_regexes(export_allow ${B_EXPORTS})
        set(export_options --use-core)
        foreach(regex ${export_allow})
            list(APPEND export_options "--allowlist-file=${regex}")
        endforeach()
        list(JOIN export_options "\n" export_content)
        file(CONFIGURE OUTPUT "${out}/${name}.exports.args" CONTENT "${export_content}\n" @ONLY)
        set(export_args "-DEXPORT_OPTIONS=${out}/${name}.exports.args")
    endif()

    set(includes "$<TARGET_PROPERTY:${COMPONENT_LIB},INCLUDE_DIRECTORIES>")
    set(definitions "$<TARGET_PROPERTY:${COMPONENT_LIB},COMPILE_DEFINITIONS>")
    file(GENERATE OUTPUT "${out}/${name}.clang.args" CONTENT
        "$<JOIN:$<LIST:TRANSFORM,${includes},PREPEND,-I>,\n>\n$<JOIN:$<LIST:TRANSFORM,${definitions},PREPEND,-D>,\n>\n")

    add_custom_command(
        OUTPUT "${gen}/bindings.rs" "${gen}/static.c"
        COMMAND "${CMAKE_COMMAND}"
                "-DBINDGEN=${bindgen}"
                "-DHEADER=${gen}/bindgen.h"
                "-DOPTIONS=${out}/${name}.bindgen.args"
                ${export_args}
                "-DCLANG_ARGS=${out}/clang.args;${out}/${name}.clang.args"
                "-DOUT_RS=${gen}/bindings.rs"
                "-DSTATIC_C=${gen}/static.c"
                "-DDEPFILE=${out}/${name}.bindgen.d"
                -P "${DEVKIT_RUST_DIR}/bindgen.cmake"
        DEPENDS "${gen}/bindgen.h" "${out}/${name}.bindgen.args" "${out}/clang.args"
                "${out}/${name}.clang.args" "${DEVKIT_RUST_DIR}/bindgen.cmake"
        DEPFILE "${out}/${name}.bindgen.d"
        COMMENT "bindgen ${name}"
        VERBATIM)
    # bindgen 0.72 writes a top-level const parameter (void *const p) into the
    # wrappers as a pointee const (const void *const p); the ABI is the same.
    set_source_files_properties("${gen}/static.c" PROPERTIES GENERATED TRUE
        COMPILE_OPTIONS "-Wno-duplicate-decl-specifier;-Wno-discarded-qualifiers")
    target_sources(${COMPONENT_LIB} PRIVATE "${gen}/static.c")
endfunction()

# devkit_rust_crate(ROOT file [EDITION year]
#                   [BINDGEN_HEADERS header...] [EXPORT_HEADERS header...])
#
# Call after idf_component_register(); the component must (PRIV_)REQUIRES
# rust_rt. Builds a crate named after the component; crates of its requirements,
# `sdkconfig`, and a --cfg per enabled bool option are in scope. The headers
# (as #included) are bound into $OUT_DIR/bindings.rs; each function declared
# in an EXPORT_HEADERS header must be defined at the crate root with the same
# signature.
function(devkit_rust_crate)
    cmake_parse_arguments(R "" "ROOT;EDITION" "BINDGEN_HEADERS;EXPORT_HEADERS" ${ARGN})
    if(NOT R_ROOT)
        message(FATAL_ERROR "devkit_rust_crate: ROOT is required")
    endif()
    if(NOT R_EDITION)
        set(R_EDITION 2024)
    endif()
    get_filename_component(root "${R_ROOT}" ABSOLUTE)

    _devkit_rust_init()
    get_property(rustc GLOBAL PROPERTY DEVKIT_RUST_RUSTC)
    idf_build_get_property(build_dir BUILD_DIR)
    set(out "${build_dir}/rust")
    set(name "${COMPONENT_NAME}")

    idf_component_get_property(reqs ${name} REQUIRES)
    idf_component_get_property(priv_reqs ${name} PRIV_REQUIRES)
    set(deps ${reqs} ${priv_reqs})
    if(NOT name STREQUAL "rust_rt" AND NOT "rust_rt" IN_LIST deps)
        message(FATAL_ERROR "devkit_rust_crate: ${name} must (PRIV_)REQUIRES rust_rt")
    endif()

    set(rmeta "${out}/lib${name}.rmeta")
    set(obj "${out}/${name}.o")
    set_property(TARGET devkit_rust PROPERTY DEVKIT_RUST_RMETA_${name} "${rmeta}")

    string(MAKE_C_IDENTIFIER "${name}" ident)
    if(NOT ident STREQUAL name)
        message(FATAL_ERROR "devkit_rust_crate: ${name} is not a valid crate name")
    endif()

    string(CONCAT args_content
        "--crate-name=${name}\n"
        "--crate-type=rlib\n"
        "--edition=${R_EDITION}\n"
        "--emit=obj,metadata=${rmeta},dep-info=${out}/${name}.rustc.d\n"
        "--out-dir=${out}/${name}.cgu\n"
        "--extern=sdkconfig=${out}/libsdkconfig.rmeta\n")
    set(dep_rmetas "")
    foreach(dep ${deps})
        set(dep_rmeta "$<TARGET_PROPERTY:devkit_rust,DEVKIT_RUST_RMETA_${dep}>")
        string(APPEND args_content "$<$<BOOL:${dep_rmeta}>:--extern=${dep}=${dep_rmeta}\n>")
        list(APPEND dep_rmetas "${dep_rmeta}")
    endforeach()
    string(APPEND args_content "${root}\n")
    file(GENERATE OUTPUT "${out}/${name}.args" CONTENT "${args_content}")

    set(gen "${out}/${name}.out")
    file(MAKE_DIRECTORY "${gen}")
    set(bindings "")
    if(R_BINDGEN_HEADERS OR R_EXPORT_HEADERS)
        _devkit_rust_bindgen(${name} "${out}" "${gen}"
            HEADERS ${R_BINDGEN_HEADERS} EXPORTS ${R_EXPORT_HEADERS})
        set(bindings "${gen}/bindings.rs")
    endif()

    add_custom_command(
        OUTPUT "${obj}" "${rmeta}"
        COMMAND "${CMAKE_COMMAND}"
                "-DRUSTC=${rustc}"
                "-DARGS=${out}/common.args;${out}/sdkconfig.args;${out}/${name}.args"
                "-DCGU_DIR=${out}/${name}.cgu"
                "-DRUSTC_DEPFILE=${out}/${name}.rustc.d"
                "-DOBJ=${obj}"
                "-DDEPFILE=${out}/${name}.d"
                "-DLINKER=${CMAKE_LINKER}"
                "-DOUT_DIR=${gen}"
                -P "${DEVKIT_RUST_DIR}/rust_crate.cmake"
        DEPENDS "${root}" "${out}/${name}.args" "${out}/common.args" "${out}/sdkconfig.args"
                "${out}/libsdkconfig.rmeta" "${DEVKIT_RUST_DIR}/rust_crate.cmake" ${dep_rmetas}
                ${bindings}
        DEPFILE "${out}/${name}.d"
        COMMENT "rustc ${name}"
        VERBATIM)
    set_source_files_properties("${obj}" PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)
    target_sources(${COMPONENT_LIB} PRIVATE "${obj}")
endfunction()
