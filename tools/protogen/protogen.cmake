set(PROTOGEN_DIR "${CMAKE_CURRENT_LIST_DIR}")
set(PROTOGEN_SCRIPTS
    "${PROTOGEN_DIR}/protogen.py"
    "${PROTOGEN_DIR}/proto.py"
    "${PROTOGEN_DIR}/c_names.py"
    "${PROTOGEN_DIR}/gen_c.py"
    "${PROTOGEN_DIR}/gen_protobuf_c.py"
    "${PROTOGEN_DIR}/gen_rust.py"
    "${PROTOGEN_DIR}/pb.rs")

# Sets <prefix>_SOURCES and <prefix>_INCLUDE_DIRS for a target defined in the
# calling directory. LANG protobuf-c leaves libprotobuf-c to the caller. LANG
# rust writes <name>.pb.rs, which includes the pb.rs written next to it, for a
# crate to include!. OUT_DIR replaces the default output directory.
function(protogen_generate prefix)
    cmake_parse_arguments(P "" "PROTO;LANG;OUT_DIR" "" ${ARGN})
    if(NOT P_PROTO OR NOT P_LANG)
        message(FATAL_ERROR "protogen_generate: PROTO and LANG are required")
    endif()
    get_filename_component(proto "${P_PROTO}" ABSOLUTE)
    get_filename_component(stem "${proto}" NAME_WE)

    set(out "${CMAKE_CURRENT_BINARY_DIR}/protogen")
    if(P_OUT_DIR)
        set(out "${P_OUT_DIR}")
    endif()
    if(P_LANG STREQUAL "c")
        set(generated "${out}/${stem}.pb.c" "${out}/${stem}.pb.h")
        set(sources ${generated} "${PROTOGEN_DIR}/pb.c")
        set(include_dirs "${out}" "${PROTOGEN_DIR}")
    elseif(P_LANG STREQUAL "rust")
        set(generated "${out}/${stem}.pb.rs" "${out}/pb.rs")
        set(sources ${generated})
        set(include_dirs "${out}")
    elseif(P_LANG STREQUAL "protobuf-c")
        set(generated "${out}/${stem}.pb-c.c" "${out}/${stem}.pb-c.h")
        set(sources ${generated})
        set(include_dirs "${out}")
    else()
        message(FATAL_ERROR "protogen_generate: unknown LANG '${P_LANG}'")
    endif()

    if(ESP_PLATFORM)
        idf_build_get_property(python PYTHON)
    else()
        find_package(Python3 REQUIRED COMPONENTS Interpreter)
        set(python "${Python3_EXECUTABLE}")
    endif()

    # IDF rejects include directories that do not exist at configure time.
    file(MAKE_DIRECTORY "${out}")
    add_custom_command(
        OUTPUT ${generated}
        COMMAND "${python}" "${PROTOGEN_DIR}/protogen.py" ${P_LANG} "${proto}" "${out}"
        DEPENDS "${proto}" ${PROTOGEN_SCRIPTS}
        COMMENT "protogen: ${stem}.proto (${P_LANG})"
        VERBATIM)
    set(${prefix}_SOURCES ${sources} PARENT_SCOPE)
    set(${prefix}_INCLUDE_DIRS ${include_dirs} PARENT_SCOPE)
endfunction()
