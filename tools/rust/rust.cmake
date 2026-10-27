set(DEVKIT_RUST_DIR "${CMAKE_CURRENT_LIST_DIR}")
cmake_policy(SET CMP0116 NEW)

# Sets <triple_out> to the Rust target and <clang_out> to the matching libclang
# target flags for bindgen. The simulator builds for the host.
function(_devkit_rust_target rustc triple_out clang_out)
    if(NOT ESP_PLATFORM)
        execute_process(
            COMMAND "${rustc}" -vV
            OUTPUT_VARIABLE version
            COMMAND_ERROR_IS_FATAL ANY)
        string(REGEX MATCH "host: ([^\n]+)" _ "${version}")
        set(triple "${CMAKE_MATCH_1}")
        set(clang --target=${triple})
    elseif(IDF_TARGET MATCHES "^esp32(s3)?$")
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
function(_devkit_rust_idf_clang_args out clang_target)
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

# The host compiler's system include directories depend on its wrapper (Nix
# injects the libc and SDK paths), so they are taken from the compiler itself.
# Its own builtin headers are left to libclang's.
function(_devkit_rust_host_clang_args out clang_target)
    execute_process(
        COMMAND "${CMAKE_C_COMPILER}" -E -Wp,-v -xc /dev/null
        OUTPUT_QUIET
        ERROR_VARIABLE search
        COMMAND_ERROR_IS_FATAL ANY)
    string(REGEX MATCH "#include <...> search starts here:\n(.*)\nEnd of search list" _ "${search}")
    string(REPLACE "\n" ";" dirs "${CMAKE_MATCH_1}")
    set(args ${clang_target} -nostdlibinc)
    foreach(dir ${dirs})
        string(STRIP "${dir}" dir)
        if(dir MATCHES " \\(framework directory\\)$")
            string(REGEX REPLACE " \\(framework directory\\)$" "" dir "${dir}")
            list(APPEND args "-iframework${dir}")
        elseif(NOT dir MATCHES "/lib/gcc/")
            list(APPEND args "-isystem${dir}")
        endif()
    endforeach()
    list(APPEND args "-std=gnu${CMAKE_C_STANDARD}")
    set(${out} ${args} PARENT_SCOPE)
endfunction()

function(_devkit_rust_requires name out)
    if(ESP_PLATFORM)
        idf_component_get_property(reqs ${name} REQUIRES)
        idf_component_get_property(priv_reqs ${name} PRIV_REQUIRES)
        set(reqs ${reqs} ${priv_reqs})
    else()
        get_property(reqs GLOBAL PROPERTY DEVKIT_SIM_REQUIRES_${name})
    endif()
    set(${out} ${reqs} PARENT_SCOPE)
endfunction()

function(_devkit_rust_out_dir out)
    if(ESP_PLATFORM)
        idf_build_get_property(build_dir BUILD_DIR)
    else()
        set(build_dir "${CMAKE_BINARY_DIR}")
    endif()
    set(${out} "${build_dir}/rust" PARENT_SCOPE)
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
    _devkit_rust_out_dir(out)
    file(MAKE_DIRECTORY "${out}")
    if(ESP_PLATFORM)
        idf_build_get_property(python PYTHON)
        idf_build_get_property(sdkconfig_json SDKCONFIG_JSON)
        idf_build_get_property(sdkconfig_menus SDKCONFIG_JSON_MENUS)
    else()
        find_package(Python3 COMPONENTS Interpreter REQUIRED)
        set(python "${Python3_EXECUTABLE}")
        set(sdkconfig_json "${CMAKE_BINARY_DIR}/config/sdkconfig.json")
        set(sdkconfig_menus "${CMAKE_BINARY_DIR}/config/kconfig_menus.json")
    endif()

    _devkit_rust_target("${rustc}" triple clang_target)
    execute_process(
        COMMAND "${rustc}" --print target-libdir "--target=${triple}"
        OUTPUT_VARIABLE libdir
        OUTPUT_STRIP_TRAILING_WHITESPACE
        COMMAND_ERROR_IS_FATAL ANY)
    set_property(GLOBAL PROPERTY DEVKIT_RUST_TARGET_LIBDIR "${libdir}")

    if(ESP_PLATFORM)
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
    else()
        if(CMAKE_BUILD_TYPE STREQUAL "Release")
            set(opt 3)
        elseif(CMAKE_BUILD_TYPE STREQUAL "RelWithDebInfo")
            set(opt 2)
        elseif(CMAKE_BUILD_TYPE STREQUAL "MinSizeRel")
            set(opt s)
        else()
            set(opt 0)
        endif()
        if(CMAKE_BUILD_TYPE MATCHES "^(Release|RelWithDebInfo|MinSizeRel)$")
            set(debug_assertions off)
        else()
            set(debug_assertions on)
        endif()
    endif()

    set(common
        "--target=${triple}"
        "-Cpanic=abort"
        "-Copt-level=${opt}"
        "-Cdebuginfo=2"
        "-Cdebug-assertions=${debug_assertions}"
        "-Zbinary-dep-depinfo"
        # For every crate rather than per crate root, as it will be once stable.
        "-Zcrate-attr=feature(allocator_api)"
        "-Aunused_features"
        "--check-cfg=cfg(test)"
        "--color=always"
        "-Ldependency=${out}")
    if(opt STREQUAL "0" OR opt STREQUAL "1")
        list(APPEND common "-Cincremental=${out}/incremental")
    endif()
    # Shared code names IDF options the simulator's Kconfig does not have; they
    # are unset here as in C's #if, and the device build still checks them.
    if(NOT ESP_PLATFORM)
        list(APPEND common "-Aunexpected_cfgs")
    endif()
    list(JOIN common "\n" common_content)
    file(CONFIGURE OUTPUT "${out}/common.args" CONTENT "${common_content}\n" @ONLY)

    find_program(bindgen bindgen REQUIRED NO_CACHE)
    set_property(GLOBAL PROPERTY DEVKIT_RUST_BINDGEN "${bindgen}")
    if(ESP_PLATFORM)
        _devkit_rust_idf_clang_args(clang_args "${clang_target}")
    else()
        _devkit_rust_host_clang_args(clang_args "${clang_target}")
    endif()
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

    # The simulator also builds rlibs: test executables are linked by rustc.
    set(emit "metadata=${out}/libsdkconfig.rmeta")
    set(outputs "${out}/libsdkconfig.rmeta")
    if(NOT ESP_PLATFORM)
        string(APPEND emit ",link=${out}/libsdkconfig.rlib")
        list(APPEND outputs "${out}/libsdkconfig.rlib")
        add_custom_target(rust_test)
    endif()
    add_custom_command(
        OUTPUT ${outputs}
        COMMAND "${rustc}" "@${out}/common.args" --edition=2024 --crate-type=rlib
                --crate-name=sdkconfig "--emit=${emit}" "${out}/sdkconfig.rs"
        DEPENDS "${out}/sdkconfig.rs" "${out}/common.args"
        COMMENT "rustc sdkconfig"
        VERBATIM)
    add_custom_target(devkit_rust_sdkconfig DEPENDS ${outputs})
    add_library(devkit_rust INTERFACE)

    execute_process(
        COMMAND "${rustc}" --print sysroot
        OUTPUT_VARIABLE sysroot
        OUTPUT_STRIP_TRAILING_WHITESPACE
        COMMAND_ERROR_IS_FATAL ANY)
    set_property(GLOBAL PROPERTY DEVKIT_RUST_SYSROOT "${sysroot}")
    set_property(GLOBAL PROPERTY DEVKIT_RUST_TRIPLE "${triple}")
    cmake_language(DEFER DIRECTORY "${CMAKE_SOURCE_DIR}" CALL _devkit_rust_project_json)
endfunction()

# Writes <build>/rust-project.json for rust-analyzer once every crate is known.
function(_devkit_rust_project_json)
    _devkit_rust_out_dir(out)
    get_property(sysroot GLOBAL PROPERTY DEVKIT_RUST_SYSROOT)
    get_property(triple GLOBAL PROPERTY DEVKIT_RUST_TRIPLE)
    get_property(crates GLOBAL PROPERTY DEVKIT_RUST_CRATES)

    file(STRINGS "${out}/sdkconfig.args" lines REGEX "^--cfg=")
    set(cfgs "[]")
    set(i 0)
    foreach(line ${lines})
        string(REPLACE "--cfg=" "" cfg "${line}")
        string(JSON cfgs SET "${cfgs}" ${i} "\"${cfg}\"")
        math(EXPR i "${i} + 1")
    endforeach()

    set(names sdkconfig ${crates})
    set(json "[]")
    set(index 0)
    foreach(name ${names})
        if(name STREQUAL "sdkconfig")
            set(root "${out}/sdkconfig.rs")
            set(edition 2024)
            set(deps "")
            set(gen "${out}")
        else()
            get_property(root GLOBAL PROPERTY DEVKIT_RUST_ROOT_${name})
            get_property(edition GLOBAL PROPERTY DEVKIT_RUST_EDITION_${name})
            get_property(deps GLOBAL PROPERTY DEVKIT_RUST_DEPS_${name})
            set(gen "${out}/${name}.out")
            list(PREPEND deps sdkconfig)
        endif()
        get_filename_component(root_dir "${root}" DIRECTORY)

        set(crate "{}")
        string(JSON crate SET "${crate}" display_name "\"${name}\"")
        string(JSON crate SET "${crate}" root_module "\"${root}\"")
        string(JSON crate SET "${crate}" edition "\"${edition}\"")
        string(JSON crate SET "${crate}" target "\"${triple}\"")
        string(JSON crate SET "${crate}" is_workspace_member true)
        string(JSON crate SET "${crate}" cfg "${cfgs}")
        string(JSON crate SET "${crate}" env "{\"OUT_DIR\": \"${gen}\"}")
        string(JSON crate SET "${crate}" source
            "{\"include_dirs\": [\"${root_dir}\", \"${gen}\"], \"exclude_dirs\": []}")
        set(crate_deps "[]")
        set(d 0)
        foreach(dep ${deps})
            list(FIND names "${dep}" dep_index)
            if(dep_index GREATER_EQUAL 0)
                string(JSON crate_deps SET "${crate_deps}" ${d}
                    "{\"crate\": ${dep_index}, \"name\": \"${dep}\"}")
                math(EXPR d "${d} + 1")
            endif()
        endforeach()
        string(JSON crate SET "${crate}" deps "${crate_deps}")
        string(JSON json SET "${json}" ${index} "${crate}")
        math(EXPR index "${index} + 1")
    endforeach()

    set(project "{}")
    string(JSON project SET "${project}" sysroot "\"${sysroot}\"")
    string(JSON project SET "${project}" sysroot_src "\"${sysroot}/lib/rustlib/src/rust/library\"")
    string(JSON project SET "${project}" crates "${json}")
    get_filename_component(build_dir "${out}" DIRECTORY)
    file(CONFIGURE OUTPUT "${build_dir}/rust-project.json" CONTENT "${project}\n" @ONLY)
endfunction()

# Host test executable for the crate's #[test]s: rust_test_<crate> builds and
# runs it, rust_test runs them all.
function(_devkit_rust_test name out gen root edition deps)
    get_property(rustc GLOBAL PROPERTY DEVKIT_RUST_RUSTC)
    set(exe "${out}/${name}-test")
    string(CONCAT args_content
        "--crate-name=${name}\n"
        "--edition=${edition}\n"
        "--test\n"
        "-Zpanic-abort-tests\n"
        "--emit=link=${exe},dep-info=${out}/${name}.test.rustc.d\n"
        "--extern=sdkconfig=${out}/libsdkconfig.rlib\n")
    set(dep_rlibs "")
    foreach(dep ${deps})
        set(dep_rlib "$<TARGET_PROPERTY:devkit_rust,DEVKIT_RUST_RLIB_${dep}>")
        string(APPEND args_content "$<$<BOOL:${dep_rlib}>:--extern=${dep}=${dep_rlib}\n>")
        list(APPEND dep_rlibs "${dep_rlib}")
    endforeach()
    set(compat devkit_sim_idf_compat)
    string(APPEND args_content
        "-Clink-arg=$<TARGET_FILE:${compat}>\n"
        "-Clink-arg=$<JOIN:$<TARGET_PROPERTY:${compat},DEVKIT_RUST_LINK_ARGS>,\n-Clink-arg=>\n"
        "${root}\n")
    file(GENERATE OUTPUT "${out}/${name}.test.args" CONTENT "${args_content}")

    add_custom_command(
        OUTPUT "${exe}"
        COMMAND "${CMAKE_COMMAND}"
                "-DRUSTC=${rustc}"
                "-DARGS=${out}/common.args;${out}/sdkconfig.args;${out}/${name}.test.args"
                "-DRUSTC_DEPFILE=${out}/${name}.test.rustc.d"
                "-DOUTPUT=${exe}"
                "-DDEPFILE=${out}/${name}.test.d"
                "-DOUT_DIR=${gen}"
                -P "${DEVKIT_RUST_DIR}/rust_crate.cmake"
        DEPENDS "${root}" "${out}/${name}.test.args" "${out}/common.args" "${out}/sdkconfig.args"
                "${out}/libsdkconfig.rlib" "${DEVKIT_RUST_DIR}/rust_crate.cmake" ${dep_rlibs}
                "$<TARGET_FILE:${compat}>" ${ARGN}
        DEPFILE "${out}/${name}.test.d"
        COMMENT "rustc --test ${name}"
        VERBATIM)
    add_custom_target(rust_test_${name} COMMAND "${exe}" DEPENDS "${exe}" USES_TERMINAL VERBATIM)
    add_dependencies(rust_test rust_test_${name})
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
    set_source_files_properties("${gen}/static.c" TARGET_DIRECTORY ${COMPONENT_LIB}
        PROPERTIES GENERATED TRUE
        COMPILE_OPTIONS "-Wno-duplicate-decl-specifier;-Wno-discarded-qualifiers")
    target_sources(${COMPONENT_LIB} PRIVATE "${gen}/static.c")
endfunction()

# devkit_rust_crate(ROOT file [EDITION year] [TESTS]
#                   [BINDGEN_HEADERS header...] [EXPORT_HEADERS header...])
#
# Call after idf_component_register(); the component must (PRIV_)REQUIRES
# rust_rt. Builds a crate named after the component; crates of its requirements,
# `sdkconfig`, and a --cfg per enabled bool option are in scope. The headers
# (as #included) are bound into $OUT_DIR/bindings.rs; each function declared
# in an EXPORT_HEADERS header must be defined at the crate root with the same
# signature. TESTS adds the simulator's rust_test_<crate> target; the crate
# root must then be #![cfg_attr(not(test), no_std)].
function(devkit_rust_crate)
    cmake_parse_arguments(R "TESTS" "ROOT;EDITION" "BINDGEN_HEADERS;EXPORT_HEADERS" ${ARGN})
    if(NOT R_ROOT)
        message(FATAL_ERROR "devkit_rust_crate: ROOT is required")
    endif()
    if(NOT R_EDITION)
        set(R_EDITION 2024)
    endif()
    get_filename_component(root "${R_ROOT}" ABSOLUTE)

    _devkit_rust_init()
    get_property(rustc GLOBAL PROPERTY DEVKIT_RUST_RUSTC)
    _devkit_rust_out_dir(out)
    set(name "${COMPONENT_NAME}")

    _devkit_rust_requires(${name} deps)
    if(NOT name STREQUAL "rust_rt" AND NOT "rust_rt" IN_LIST deps)
        message(FATAL_ERROR "devkit_rust_crate: ${name} must (PRIV_)REQUIRES rust_rt")
    endif()

    set(rmeta "${out}/lib${name}.rmeta")
    set(obj "${out}/${name}.o")
    set_property(TARGET devkit_rust PROPERTY DEVKIT_RUST_RMETA_${name} "${rmeta}")
    set_property(GLOBAL APPEND PROPERTY DEVKIT_RUST_CRATES ${name})
    set_property(GLOBAL PROPERTY DEVKIT_RUST_ROOT_${name} "${root}")
    set_property(GLOBAL PROPERTY DEVKIT_RUST_EDITION_${name} ${R_EDITION})
    set_property(GLOBAL PROPERTY DEVKIT_RUST_DEPS_${name} ${deps})
    set(emit "obj,metadata=${rmeta},dep-info=${out}/${name}.rustc.d")
    set(outputs "${obj}" "${rmeta}")
    if(NOT ESP_PLATFORM)
        set(rlib "${out}/lib${name}.rlib")
        set_property(TARGET devkit_rust PROPERTY DEVKIT_RUST_RLIB_${name} "${rlib}")
        string(APPEND emit ",link=${rlib}")
        list(APPEND outputs "${rlib}")
    endif()

    string(MAKE_C_IDENTIFIER "${name}" ident)
    if(NOT ident STREQUAL name)
        message(FATAL_ERROR "devkit_rust_crate: ${name} is not a valid crate name")
    endif()

    string(CONCAT args_content
        "--crate-name=${name}\n"
        "--crate-type=rlib\n"
        "--edition=${R_EDITION}\n"
        "--emit=${emit}\n"
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
        OUTPUT ${outputs}
        COMMAND "${CMAKE_COMMAND}"
                "-DRUSTC=${rustc}"
                "-DARGS=${out}/common.args;${out}/sdkconfig.args;${out}/${name}.args"
                "-DCGU_DIR=${out}/${name}.cgu"
                "-DRUSTC_DEPFILE=${out}/${name}.rustc.d"
                "-DOUTPUT=${obj}"
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
    # The simulator shim adds sources to a target defined in another directory,
    # where custom command outputs are neither GENERATED nor attached to a rule.
    if(NOT ESP_PLATFORM)
        add_custom_target(devkit_rust_${name} DEPENDS "${obj}" ${bindings})
        add_dependencies(${COMPONENT_LIB} devkit_rust_${name})
    endif()
    set_source_files_properties("${obj}" TARGET_DIRECTORY ${COMPONENT_LIB}
        PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)
    target_sources(${COMPONENT_LIB} PRIVATE "${obj}")

    if(R_TESTS AND NOT ESP_PLATFORM)
        _devkit_rust_test(${name} "${out}" "${gen}" "${root}" ${R_EDITION} "${deps}" ${bindings})
    endif()
endfunction()
