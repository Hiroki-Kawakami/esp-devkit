set(IMGF_SRCS
        "src/imgf_alloc.c"
        "src/imgf_async.c"
        "src/imgf_breader.c"
        "src/imgf_decoder.c"
        "src/imgf_dither.c"
        "src/imgf_encoder.c"
        "src/imgf_err.c"
        "src/imgf_inflate.c"
        "src/imgf_jpegd.c"
        "src/imgf_jpege.c"
        "src/imgf_pngd.c"
        "src/imgf_raw_encoder.c"
        "src/imgf_recolor.c"
        "src/imgf_resize.c"
        "src/imgf_sniff.c"
        "src/imgf_stream.c")

if(IDF_TARGET STREQUAL "esp32p4")
    list(APPEND IMGF_SRCS "src/imgf_pie_p4.S")
endif()

idf_component_register(
    SRCS ${IMGF_SRCS}
    INCLUDE_DIRS "inc"
    PRIV_INCLUDE_DIRS "src"
    PRIV_REQUIRES heap freertos)

# A codec is where the size and debug optimization levels cost the most: the
# decode and resize loops run 1.4-1.6x slower on ESP32-P4 than at -O2, for 2 KB
# of flash. A project that asked for -O0 or -O2 outright keeps what it asked
# for.
if(ESP_PLATFORM AND NOT CONFIG_COMPILER_OPTIMIZATION_NONE
                AND NOT CONFIG_COMPILER_OPTIMIZATION_PERF)
    target_compile_options(${COMPONENT_LIB} PRIVATE -O2)
endif()
