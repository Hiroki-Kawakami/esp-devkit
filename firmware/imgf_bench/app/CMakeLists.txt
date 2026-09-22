# Shared imgf_bench app component.
idf_component_register(
    SRCS "imgf_bench.c"
    INCLUDE_DIRS "." "${CMAKE_CURRENT_BINARY_DIR}"
    REQUIRES image_framework esp_timer)

# Everything below runs only in the real configure pass: idf_component_register
# returns early while IDF is expanding component requirements in script mode,
# where add_custom_command does not exist.
if(NOT DEFINED PYTHON)
    set(PYTHON python3)
endif()

set(BENCH_ASSETS "${CMAKE_CURRENT_BINARY_DIR}/imgf_bench_assets.h")
add_custom_command(
    OUTPUT ${BENCH_ASSETS}
    COMMAND ${PYTHON} "${CMAKE_CURRENT_SOURCE_DIR}/gen_assets.py" ${BENCH_ASSETS} 960 540
    DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/gen_assets.py"
    VERBATIM)
add_custom_target(imgf_bench_assets DEPENDS ${BENCH_ASSETS})
add_dependencies(${COMPONENT_LIB} imgf_bench_assets)
