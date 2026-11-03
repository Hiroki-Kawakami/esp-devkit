set(PBCHECK_DIR "${CMAKE_CURRENT_LIST_DIR}")
include("${PBCHECK_DIR}/../protogen.cmake")

# RUST_TEST adds <TARGET>_rs.test, a rustc --test of that file against the messages
# <TARGET> --dump writes; it includes pbcheck.rs and $PBCHECK_GEN/<name>.pb.rs.
function(pbcheck_add_executable)
    cmake_parse_arguments(P "" "TARGET;PROTO;RUST_TEST" "SOURCES" ${ARGN})
    if(NOT DEFINED ENV{IDF_PATH})
        message(FATAL_ERROR "pbcheck: IDF_PATH is not set; run the build through `nix develop`")
    endif()
    set(protobuf_c "$ENV{IDF_PATH}/components/protobuf-c/protobuf-c")
    protogen_generate(pb LANG c PROTO "${P_PROTO}")
    protogen_generate(ref LANG protobuf-c PROTO "${P_PROTO}")
    add_executable(${P_TARGET} ${P_SOURCES} ${pb_SOURCES} ${ref_SOURCES}
        "${PBCHECK_DIR}/pbcheck.c" "${protobuf_c}/protobuf-c/protobuf-c.c")
    target_include_directories(${P_TARGET} PRIVATE
        "${PBCHECK_DIR}" "${protobuf_c}" ${pb_INCLUDE_DIRS} ${ref_INCLUDE_DIRS})
    target_compile_options(${P_TARGET} PRIVATE -Wall -Wextra)

    if(P_RUST_TEST)
        find_program(RUSTC rustc REQUIRED)
        get_filename_component(rust_test "${P_RUST_TEST}" ABSOLUTE)
        set(gen "${CMAKE_CURRENT_BINARY_DIR}/protogen_rust")
        protogen_generate(rs LANG rust PROTO "${P_PROTO}" OUT_DIR "${gen}")
        set(fixture "${CMAKE_CURRENT_BINARY_DIR}/${P_TARGET}.fixture")
        add_custom_command(OUTPUT "${fixture}"
            COMMAND ${P_TARGET} --dump "${fixture}"
            DEPENDS ${P_TARGET}
            VERBATIM)
        set(exe "${CMAKE_CURRENT_BINARY_DIR}/${P_TARGET}_rs.test")
        add_custom_command(OUTPUT "${exe}"
            COMMAND "${CMAKE_COMMAND}" -E env "PBCHECK_GEN=${gen}" "PBCHECK_FIXTURE=${fixture}"
                    "${RUSTC}" --edition 2024 --test -Zpanic-abort-tests -Cpanic=abort -o "${exe}" "${rust_test}"
            DEPENDS "${rust_test}" "${PBCHECK_DIR}/pbcheck.rs" ${rs_SOURCES} "${fixture}"
            VERBATIM)
        add_custom_target(${P_TARGET}_rs ALL DEPENDS "${exe}")
    endif()
endfunction()
