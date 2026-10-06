set(AUDF_SRCS
        "src/audf_aac.c"
        "src/audf_aac_ics.c"
        "src/audf_aac_imdct.c"
        "src/audf_aac_kernels_c.c"
        "src/audf_aac_ps.c"
        "src/audf_aac_qmf.c"
        "src/audf_aac_sbr.c"
        "src/audf_aac_sbr_hf.c"
        "src/audf_aac_tables.c"
        "src/audf_adpcm.c"
        "src/audf_alloc.c"
        "src/audf_codec.c"
        "src/audf_convert.c"
        "src/audf_drift.c"
        "src/audf_eq.c"
        "src/audf_fifo.c"
        "src/audf_gain.c"
        "src/audf_graph.c"
        "src/audf_mixer.c"
        "src/audf_resampler.c"
        "src/audf_sync.c")

if(IDF_TARGET STREQUAL "esp32p4")
    list(APPEND AUDF_SRCS "src/audf_aac_pie_p4.S")
endif()

idf_component_register(
    SRCS ${AUDF_SRCS}
    INCLUDE_DIRS "inc"
    PRIV_INCLUDE_DIRS "src"
    PRIV_REQUIRES freertos)

# The AAC decoder's float control path (TNS, SBR, PS) must round the
# same on the host and the device, which a fused multiply-add would break.
set(AUDF_AAC_SRCS ${AUDF_SRCS})
list(FILTER AUDF_AAC_SRCS INCLUDE REGEX "audf_aac")
set(AUDF_AAC_OPTIONS -ffp-contract=off -fno-math-errno)
if(ESP_PLATFORM AND NOT CONFIG_COMPILER_OPTIMIZATION_NONE
                AND NOT CONFIG_COMPILER_OPTIMIZATION_PERF)
    list(APPEND AUDF_AAC_OPTIONS -O2)
endif()
set_source_files_properties(${AUDF_AAC_SRCS} PROPERTIES COMPILE_OPTIONS "${AUDF_AAC_OPTIONS}")
