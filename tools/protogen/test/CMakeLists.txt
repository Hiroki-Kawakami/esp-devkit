cmake_minimum_required(VERSION 3.16)
project(protogen_test C)

include(pbcheck.cmake)
pbcheck_add_executable(TARGET protogen_test PROTO test.proto SOURCES test_main.c)
