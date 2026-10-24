idf_component_register(
    SRCS "src/cmpf_inflate.c" "src/cmpf_lzw.c"
    INCLUDE_DIRS "inc")

# The decode loops are the whole of this component; a project that asked for
# -O0 or -O2 outright keeps what it asked for.
if(ESP_PLATFORM AND NOT CONFIG_COMPILER_OPTIMIZATION_NONE
                AND NOT CONFIG_COMPILER_OPTIMIZATION_PERF)
    target_compile_options(${COMPONENT_LIB} PRIVATE -O2)
endif()
