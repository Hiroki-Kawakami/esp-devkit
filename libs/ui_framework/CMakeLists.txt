# esp-devkit ui_framework — LVGL port + reusable UI utilities (lvgl++ helpers in
# lvgl.hpp/.cpp, ScreenManager/Screen, and the optional BSP-backed
# DisplayManager). LVGL itself is the lvgl component (libs/lvgl).

set(UI_FRAMEWORK_SRCS
    "src/display_manager.cpp"
    "src/lvgl.cpp"
    "src/packed_font.cpp"
    "src/screen_manager.cpp"
    "src/widgets/common/fonts.cpp"
    "src/widgets/common/layout.cpp")

if(CONFIG_UI_FRAMEWORK_WIDGETS_PAPER)
    list(APPEND UI_FRAMEWORK_SRCS
        "src/widgets/paper/button.cpp"
        "src/widgets/paper/fonts.cpp"
        "src/widgets/paper/list.cpp"
        "src/widgets/paper/modal.cpp"
        "src/widgets/paper/navigation.cpp")
elseif(CONFIG_UI_FRAMEWORK_WIDGETS_TAB5)
    list(APPEND UI_FRAMEWORK_SRCS
        "src/widgets/tab5/button.cpp"
        "src/widgets/tab5/fonts.cpp"
        "src/widgets/tab5/layout.cpp"
        "src/widgets/tab5/list.cpp"
        "src/widgets/tab5/modal.cpp"
        "src/widgets/tab5/navigation.cpp")
endif()

idf_component_register(
    SRCS ${UI_FRAMEWORK_SRCS}
    INCLUDE_DIRS "inc"
    REQUIRES bsp lvgl
    PRIV_REQUIRES esp_driver_ppa esp_timer harness)

if(ESP_PLATFORM)
    # Link every member so strong overrides replace the common weak defaults; from a
    # plain archive the linker stops at the first (weak) definition and never
    # extracts the override.
    idf_component_set_property(${COMPONENT_NAME} WHOLE_ARCHIVE ON)

    # Route every lv_async_call() through lvgl.cpp's __wrap_ version, which wakes
    # the LVGL task so the queued call runs without waiting for its next timer.
    target_link_libraries(${COMPONENT_LIB} INTERFACE "-Wl,--wrap=lv_async_call")
endif()
