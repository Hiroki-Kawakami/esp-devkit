cmake_minimum_required(VERSION 3.16)
project(hosted_rpc_test C)

include(../../../tools/protogen/test/pbcheck.cmake)
pbcheck_add_executable(TARGET hosted_rpc_test
    PROTO ../third_party/esp_hosted_rpc/esp_hosted_rpc.proto
    SOURCES test_main.c)
