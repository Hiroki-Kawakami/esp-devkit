# esp-devkit bsp — shared board-support component.
#
# The model-agnostic glue (src/) and the simulator's SDL backend are built
# unconditionally; everything board-specific (which chip drivers, include dirs,
# requirements, and the per-target bring-up files) comes from the selected
# board's boards/<board>/board.cmake. The board is chosen at configure time by
# BSP_BOARD, resolved from:
#   1. an explicit set(BSP_BOARD ...) — the simulator, or a build-root override;
#   2. Kconfig CONFIG_BSP_BOARD_* — device builds pick the board via sdkconfig,
#      so several boards can share one IDF_TARGET.
# With neither set the configure fails, asking for a board to be selected.
# There is no runtime board detection: one target builds one board.
#
# The build selects device vs. simulator by target: ESP_PLATFORM is defined only
# under ESP-IDF (device); the simulator consumes this file through the
# idf_component_register shim in simulator/CMakeLists.txt, where ESP_PLATFORM is
# unset.

# Model-agnostic display/touch/rtc dispatch — built for both targets.
set(BSP_COMMON_SRCS
    "src/bsp_display.c"
    "src/bsp_input.c"
    "src/bsp_touch.c"
    "src/bsp_button.c"
    "src/bsp_hotknot.c"
    "src/bsp_rtc.c"
    "src/bsp_power.c"
    "src/bsp_led.c"
    "src/bsp_audio.c"
    "src/audio_dsp.c")

if(CMAKE_BUILD_EARLY_EXPANSION)
    # IDF extracts each component's requirements before Kconfig runs, so the
    # concrete board (CONFIG_BSP_BOARD_*) is not knowable here. Only PRIV_REQUIRES
    # matters at this phase, so declare the union of every same-target board's
    # requirements -- the dependency graph then covers whichever board the real
    # configure pass selects. SRCS/includes are re-evaluated then and irrelevant now.
    set(BOARD_DEVICE_SRCS "")
    set(BOARD_DEVICE_PRIV_INCLUDE_DIRS "")
    set(_bsp_union_reqs "")
    file(GLOB _bsp_board_cmakes "${CMAKE_CURRENT_LIST_DIR}/boards/*/board.cmake")
    foreach(_bsp_cmake ${_bsp_board_cmakes})
        include("${_bsp_cmake}")
        if(BOARD_TARGET STREQUAL IDF_TARGET)
            list(APPEND _bsp_union_reqs ${BOARD_DEVICE_PRIV_REQUIRES})
        endif()
    endforeach()
    list(REMOVE_DUPLICATES _bsp_union_reqs)
    set(BOARD_DEVICE_PRIV_REQUIRES ${_bsp_union_reqs})
else()
    # Real configure pass. The simulator and build-root overrides set BSP_BOARD
    # explicitly; device builds leave it to Kconfig.
    if(NOT DEFINED BSP_BOARD)
        if(CONFIG_BSP_BOARD_PAPER)
            set(BSP_BOARD paper)
        elseif(CONFIG_BSP_BOARD_ATOM_LITE)
            set(BSP_BOARD atom_lite)
        elseif(CONFIG_BSP_BOARD_PAPER_S3)
            set(BSP_BOARD paper_s3)
        elseif(CONFIG_BSP_BOARD_ATOM_S3_LITE)
            set(BSP_BOARD atom_s3_lite)
        elseif(CONFIG_BSP_BOARD_AIRQ)
            set(BSP_BOARD airq)
        elseif(CONFIG_BSP_BOARD_TAB5)
            set(BSP_BOARD tab5)
        elseif(CONFIG_BSP_BOARD_S31_KORVO)
            set(BSP_BOARD s31_korvo)
        else()
            message(FATAL_ERROR
                "esp-devkit bsp: no board selected for IDF_TARGET '${IDF_TARGET}'. "
                "Choose one under 'Board Support Package -> Target board' in "
                "menuconfig (or set CONFIG_BSP_BOARD_* in sdkconfig.defaults).")
        endif()
    endif()
    # board.cmake fills BOARD_TARGET, BOARD_DEVICE_SRCS /
    # BOARD_DEVICE_PRIV_INCLUDE_DIRS / BOARD_DEVICE_PRIV_REQUIRES (device) and
    # BOARD_SIM_SRCS (simulator).
    include("${CMAKE_CURRENT_LIST_DIR}/boards/${BSP_BOARD}/board.cmake")
endif()

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
            "simulator/power_sim.c"
            "simulator/sdl_audio.c"
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "inc_private" "simulator")
endif()
