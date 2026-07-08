# idf_component_register()/idf_component_set_property() shims for the
# simulator build. Kept in their own file (rather than defined inline in the
# devkit_simulator macro) because CMake macros text-substitute their formal
# parameters (${ARGN}, ...) into anything textually inside them, including
# function bodies defined there. That would corrupt these functions' own
# ${ARGN} with the macro caller's arguments. Functions defined via include()
# are not subject to that substitution.

# Both the simulator-only layer (idf_compat) and the shared components
# describe themselves with the ESP-IDF idf_component_register() call in
# their own CMakeLists.txt. On device that is the real IDF function; here
# we shim it to fold the component's sources / include dirs straight into
# the `simulator` executable.
#
# REQUIRES are intentionally ignored: everything links into a single
# binary so include dirs are global, and IDF-only requirements
# (esp_lvgl_port, driver, nvs_flash, ...) simply have no host counterpart.
function(idf_component_register)
    cmake_parse_arguments(C "" ""
        "SRCS;INCLUDE_DIRS;PRIV_INCLUDE_DIRS;REQUIRES;PRIV_REQUIRES;EMBED_FILES;EMBED_TXTFILES"
        ${ARGN})
    foreach(src ${C_SRCS})
        if(NOT IS_ABSOLUTE "${src}")
            set(src "${CMAKE_CURRENT_SOURCE_DIR}/${src}")
        endif()
        target_sources(simulator PRIVATE "${src}")
    endforeach()
    foreach(dir ${C_INCLUDE_DIRS} ${C_PRIV_INCLUDE_DIRS})
        if(NOT IS_ABSOLUTE "${dir}")
            set(dir "${CMAKE_CURRENT_SOURCE_DIR}/${dir}")
        endif()
        target_include_directories(simulator PRIVATE "${dir}")
    endforeach()
endfunction()

# No-op stub for the other IDF component-CMake call shared components may use.
function(idf_component_set_property)
endfunction()
