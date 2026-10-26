# cmake -P step behind devkit_simulator(): reads each component's REQUIRES and
# PRIV_REQUIRES the way IDF's early expansion does, so only components the
# firmware reaches are added. Writes "<name> <requirement>..." lines to OUT.
#   -DCOMPONENT_PATHS=<dir;...> -DSDKCONFIG_CMAKE=<file> -DOUT=<file>

include("${SDKCONFIG_CMAKE}")
set(CMAKE_BUILD_EARLY_EXPANSION 1)

macro(idf_component_register)
    cmake_parse_arguments(_req "" "" "REQUIRES;PRIV_REQUIRES" ${ARGN})
    set(__requires ${_req_REQUIRES} ${_req_PRIV_REQUIRES})
    set(__registered TRUE)
    return()
endmacro()

function(_scan dir)
    set(CMAKE_CURRENT_SOURCE_DIR "${dir}")
    set(CMAKE_CURRENT_LIST_DIR "${dir}")
    set(__requires "")
    set(__registered FALSE)
    include("${dir}/CMakeLists.txt")
    if(NOT __registered)
        message(FATAL_ERROR "${dir}/CMakeLists.txt does not call idf_component_register()")
    endif()
    get_filename_component(name "${dir}" NAME)
    list(JOIN __requires " " reqs)
    file(APPEND "${OUT}" "${name} ${reqs}\n")
endfunction()

file(WRITE "${OUT}" "")
foreach(dir ${COMPONENT_PATHS})
    _scan("${dir}")
endforeach()
