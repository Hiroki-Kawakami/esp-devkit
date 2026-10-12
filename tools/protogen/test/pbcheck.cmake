set(PBCHECK_DIR "${CMAKE_CURRENT_LIST_DIR}")
include("${PBCHECK_DIR}/../protogen.cmake")

function(pbcheck_add_executable)
    cmake_parse_arguments(P "" "TARGET;PROTO" "SOURCES" ${ARGN})
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
endfunction()
