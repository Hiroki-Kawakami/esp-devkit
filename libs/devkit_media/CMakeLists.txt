# Media file containers: a read-ahead file buffer, tags and demuxers.
idf_component_register(SRCS "/dev/null" PRIV_REQUIRES devkit idf rust_rt)
devkit_rust_crate(ROOT "src/lib.rs" TESTS OPT_LEVEL 2)
