# Async USB host stack.
set(srcs "/dev/null")
set(headers "")
if(NOT ESP_PLATFORM)
    set(srcs "src/sim/fd.c")
    set(headers "stdlib.h" "sys/socket.h" "unistd.h" "fd.h")
endif()
idf_component_register(SRCS ${srcs} PRIV_INCLUDE_DIRS "src/sim" PRIV_REQUIRES devkit idf rust_rt)
devkit_rust_crate(ROOT "src/lib.rs" TESTS BINDGEN_HEADERS ${headers})
