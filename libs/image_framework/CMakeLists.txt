idf_component_register(
    SRCS
        "src/imgf_alloc.c"
        "src/imgf_breader.c"
        "src/imgf_decoder.c"
        "src/imgf_dither.c"
        "src/imgf_encoder.c"
        "src/imgf_err.c"
        "src/imgf_inflate.c"
        "src/imgf_jpegd.c"
        "src/imgf_pngd.c"
        "src/imgf_resize.c"
        "src/imgf_sniff.c"
        "src/imgf_stream.c"
    INCLUDE_DIRS "inc"
    PRIV_INCLUDE_DIRS "src"
    PRIV_REQUIRES heap)
