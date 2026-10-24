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
        "src/audf_alac.c"
        "src/audf_alloc.c"
        "src/audf_codec.c"
        "src/audf_convert.c"
        "src/audf_drift.c"
        "src/audf_flac.c"
        "src/audf_eq.c"
        "src/audf_fifo.c"
        "src/audf_gain.c"
        "src/audf_graph.c"
        "src/audf_mixer.c"
        "src/audf_mp3.c"
        "src/audf_mp3_l2.c"
        "src/audf_mp3_l3.c"
        "src/audf_mp3_tables.c"
        "src/audf_opus.c"
        "src/audf_opus_bands.c"
        "src/audf_opus_celt.c"
        "src/audf_opus_mdct.c"
        "src/audf_opus_rc.c"
        "src/audf_opus_silk.c"
        "src/audf_opus_silk_tables.c"
        "src/audf_opus_tables.c"
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

# The AAC and Opus decoders' float control paths (TNS, SBR, PS; CELT band
# gains) must round the same on the host and the device, which a fused
# multiply-add would break.
set(AUDF_FLOAT_SRCS ${AUDF_SRCS})
list(FILTER AUDF_FLOAT_SRCS INCLUDE REGEX "audf_aac|audf_opus")
set(AUDF_FLOAT_OPTIONS -ffp-contract=off -fno-math-errno)
if(ESP_PLATFORM AND NOT CONFIG_COMPILER_OPTIMIZATION_NONE
                AND NOT CONFIG_COMPILER_OPTIMIZATION_PERF)
    list(APPEND AUDF_FLOAT_OPTIONS -O2)
endif()
set_source_files_properties(${AUDF_FLOAT_SRCS} PROPERTIES COMPILE_OPTIONS "${AUDF_FLOAT_OPTIONS}")
