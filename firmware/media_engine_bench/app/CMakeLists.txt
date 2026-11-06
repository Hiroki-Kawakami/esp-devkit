# Checks media_engine against the IDF drivers on the same pictures.
idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES devkit idf media_engine rust_rt)
devkit_rust_crate(ROOT "lib.rs")
