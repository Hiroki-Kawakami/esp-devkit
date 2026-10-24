set(MEDF_SRCS
        "src/medf_alac.c"
        "src/medf_avi_demux.c"
        "src/medf_buffer.c"
        "src/medf_es_demux.c"
        "src/medf_flac_demux.c"
        "src/medf_id3.c"
        "src/medf_mkv_demux.c"
        "src/medf_mp4_demux.c"
        "src/medf_riff.c"
        "src/medf_tags.c"
        "src/medf_vorbis.c"
        "src/medf_wav_demux.c")

idf_component_register(
    SRCS ${MEDF_SRCS}
    INCLUDE_DIRS "inc"
    PRIV_INCLUDE_DIRS "src"
    PRIV_REQUIRES heap freertos)
