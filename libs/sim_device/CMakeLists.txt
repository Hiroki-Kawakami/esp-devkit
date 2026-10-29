# Firmware side of the simulator host link (simulator builds only).
idf_component_register(
    SRCS "/dev/null"
    REQUIRES idf
    PRIV_REQUIRES rust_rt)
devkit_rust_crate(ROOT "src/lib.rs" TESTS
    BINDGEN_HEADERS "simulator/sim_psram.h" "stdlib.h" "unistd.h" "sys/socket.h")
