# esp-devkit bsp — shared board-support component.
#
# The model-agnostic glue (src/) and the simulator's SDL backend are built
# unconditionally; everything board-specific (which chip drivers, include dirs,
# requirements, and the per-target bring-up files) comes from the selected
# board's boards/<board>/board.cmake. The board is chosen at configure time by
# BSP_BOARD — set by each build root (esp32s3/ -> paper_s3, esp32/ -> paper) and
# by simulator/CMakeLists.txt; it defaults to paper_s3 here. There is no runtime
# board detection: one target builds one board.
#
# The build selects device vs. simulator by target: ESP_PLATFORM is defined only
# under ESP-IDF (device); the simulator consumes this file through the
# idf_component_register shim in simulator/CMakeLists.txt, where ESP_PLATFORM is
# unset.

# Model-agnostic display/touch/rtc dispatch — built for both targets.
set(BSP_COMMON_SRCS
    "src/bsp_display.c"
    "src/bsp_touch.c"
    "src/bsp_hotknot.c"
    "src/bsp_rtc.c")

if(NOT DEFINED BSP_BOARD)
    set(BSP_BOARD paper_s3)
endif()

# board.cmake fills BOARD_DEVICE_SRCS / BOARD_DEVICE_PRIV_INCLUDE_DIRS /
# BOARD_DEVICE_PRIV_REQUIRES (device) and BOARD_SIM_SRCS (simulator).
include("${CMAKE_CURRENT_LIST_DIR}/boards/${BSP_BOARD}/board.cmake")

if(ESP_PLATFORM)
    idf_component_register(
        SRCS
            ${BSP_COMMON_SRCS}
            ${BOARD_DEVICE_SRCS}
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "inc_private" ${BOARD_DEVICE_PRIV_INCLUDE_DIRS}
        PRIV_REQUIRES ${BOARD_DEVICE_PRIV_REQUIRES})

    # Enable Link Time Optimization
    target_compile_options(${COMPONENT_LIB} PRIVATE -flto)
    target_link_libraries(${COMPONENT_LIB} PRIVATE -flto)
else()
    # Simulator: SDL backend + the selected board's simulator bring-up. SDL2 is
    # already linked to the `simulator` executable (see simulator/CMakeLists.txt),
    # so its headers are on the global include path here.
    idf_component_register(
        SRCS
            ${BSP_COMMON_SRCS}
            ${BOARD_SIM_SRCS}
            "simulator/sdl_panel.c"
            "simulator/sd_redirect.c"
            "simulator/rtc_sim.c"
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "inc_private" "simulator")
endif()
