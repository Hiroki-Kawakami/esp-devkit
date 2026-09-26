idf_component_register(
    SRCS
        "third_party/esp_hosted_rpc/esp_hosted_rpc.pb-c.c"
        "third_party/protobuf-c/protobuf-c/protobuf-c.c"
    INCLUDE_DIRS
        "inc"
        "third_party/esp_hosted_rpc"
        "third_party/protobuf-c")
