set(DEVKIT_RUST_DIR "${CMAKE_CURRENT_LIST_DIR}")
cmake_policy(SET CMP0116 NEW)

function(_devkit_rust_triple out)
    if(IDF_TARGET STREQUAL "esp32")
        set(triple xtensa-esp32-none-elf)
    elseif(IDF_TARGET STREQUAL "esp32s3")
        set(triple xtensa-esp32s3-none-elf)
    elseif(IDF_TARGET MATCHES "^esp32(p4|s31)$")
        set(triple riscv32imafc-unknown-none-elf)
    elseif(IDF_TARGET MATCHES "^esp32(c5|c6|c61|h2|h21)$")
        set(triple riscv32imac-unknown-none-elf)
    else()
        message(FATAL_ERROR "devkit_rust_crate: no Rust target for ${IDF_TARGET}")
    endif()
    set(${out} ${triple} PARENT_SCOPE)
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

    _devkit_rust_triple(triple)
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

# devkit_rust_crate(ROOT file [EDITION year])
#
# Call after idf_component_register(); the component must (PRIV_)REQUIRES
# rust_rt. Builds a crate named after the component; crates of its requirements,
# `sdkconfig`, and a --cfg per enabled bool option are in scope.
function(devkit_rust_crate)
    cmake_parse_arguments(R "" "ROOT;EDITION" "" ${ARGN})
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
                -P "${DEVKIT_RUST_DIR}/rust_crate.cmake"
        DEPENDS "${root}" "${out}/${name}.args" "${out}/common.args" "${out}/sdkconfig.args"
                "${out}/libsdkconfig.rmeta" "${DEVKIT_RUST_DIR}/rust_crate.cmake" ${dep_rmetas}
        DEPFILE "${out}/${name}.d"
        COMMENT "rustc ${name}"
        VERBATIM)
    set_source_files_properties("${obj}" PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)
    target_sources(${COMPONENT_LIB} PRIVATE "${obj}")
endfunction()
