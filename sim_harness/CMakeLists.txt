# esp-devkit sim_harness — host-only scripted UI verification harness.
#
# Simulator-only: it is consumed through the idf_component_register shim in
# simulator/CMakeLists.txt (ESP_PLATFORM unset). sim_harness.c has no SDL / LVGL
# / BSP / app coupling — capabilities are injected at runtime (frame capture and
# its image encoding are the platform's job, e.g. sdl_panel). On device there is
# no host harness, so the component registers nothing.
if(ESP_PLATFORM)
    idf_component_register()
else()
    idf_component_register(
        SRCS
            "sim_harness.c"
        INCLUDE_DIRS ".")
endif()
