# esp-devkit ui_framework — LVGL port abstraction + reusable UI utilities
# (lvgl++ helpers in lvgl.hpp/.cpp, ScreenManager/Screen). Panel-agnostic: the
# BSP<->LVGL binding lives in the app, not here.
#
# Both targets share the same sources. The per-target LVGL-port split is fenced
# behind ESP_PLATFORM inside lvgl.{hpp,cpp}: the app calls lvgl_port_init() the
# same way on both, and what differs is where LVGL comes from / how it runs:
#
#   - device (ESP_PLATFORM): the real esp_lvgl_port is the managed dependency
#     (see idf_component.yml); LVGL is configured via sdkconfig.
#   - simulator: original LVGL is fetched here with FetchContent and linked into
#     the `simulator` executable; lvgl.cpp supplies a lvgl_port_init/lvgl_sim_loop
#     shim. LVGL is configured by the in-tree default sim/lv_conf.h — override
#     the config dir via the UI_FRAMEWORK_LV_CONF_DIR cache var before configuring.
#
# Only the major.minor of the two LVGL versions need agree — the API surface used
# here is stable across 9.x; an exact match is not required.

set(UI_FRAMEWORK_SRCS
    "src/lvgl.cpp"
    "src/screen_manager.cpp"
    "src/widgets/modal.cpp"
    "src/widgets/navigation.cpp")

if(ESP_PLATFORM)
    idf_component_register(
        SRCS ${UI_FRAMEWORK_SRCS}
        INCLUDE_DIRS "inc"
        REQUIRES esp_lvgl_port)
else()
    # Fetch original LVGL for the host build. release/v9.5 tracks the same
    # major.minor as the device's managed lvgl (esp_lvgl_port pulls 9.x).
    include(FetchContent)

    # Default lv_conf lives in-tree; a simulator may override the directory by
    # setting UI_FRAMEWORK_LV_CONF_DIR before add_subdirectory(ui_framework).
    # LVGL's desktop CMake reads LV_BUILD_CONF_DIR: it adds the dir to LVGL's
    # public includes, defines LV_CONF_INCLUDE_SIMPLE, and checks lv_conf.h
    # exists there — so this must be set before FetchContent_MakeAvailable.
    if(NOT UI_FRAMEWORK_LV_CONF_DIR)
        set(UI_FRAMEWORK_LV_CONF_DIR "${CMAKE_CURRENT_SOURCE_DIR}/sim")
    endif()
    set(LV_BUILD_CONF_DIR "${UI_FRAMEWORK_LV_CONF_DIR}" CACHE PATH "" FORCE)
    # We only need the core library — skip the bundled examples/demos/ThorVG.
    set(CONFIG_LV_BUILD_EXAMPLES     OFF CACHE BOOL "" FORCE)
    set(CONFIG_LV_BUILD_DEMOS        OFF CACHE BOOL "" FORCE)
    set(CONFIG_LV_USE_THORVG_INTERNAL OFF CACHE BOOL "" FORCE)
    FetchContent_Declare(
        lvgl
        GIT_REPOSITORY https://github.com/lvgl/lvgl.git
        GIT_TAG        release/v9.5)
    FetchContent_MakeAvailable(lvgl)

    # The simulator executable (created in simulator/CMakeLists.txt before
    # components are added) links LVGL; its headers then reach every folded
    # component source via include propagation. Plain signature to match the
    # other target_link_libraries(simulator ...) call in simulator/CMakeLists.txt.
    target_link_libraries(simulator lvgl)

    # Fold ui_framework's own sources/includes into `simulator` via the shim.
    idf_component_register(
        SRCS ${UI_FRAMEWORK_SRCS}
        INCLUDE_DIRS "inc")
endif()
