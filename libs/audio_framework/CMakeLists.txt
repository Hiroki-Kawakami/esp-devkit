idf_component_register(
    SRCS
        "src/audf_adpcm.c"
        "src/audf_codec.c"
        "src/audf_convert.c"
        "src/audf_drift.c"
        "src/audf_eq.c"
        "src/audf_fifo.c"
        "src/audf_gain.c"
        "src/audf_graph.c"
        "src/audf_mixer.c"
        "src/audf_resampler.c"
        "src/audf_sync.c"
    INCLUDE_DIRS "inc"
    PRIV_INCLUDE_DIRS "src"
    PRIV_REQUIRES freertos)
