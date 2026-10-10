# Upstream LVGL, downloaded into the build directory the first time this
# component is part of a build, configured by Kconfig in this directory.

if(CMAKE_BUILD_EARLY_EXPANSION)
    idf_component_register()
    return()
endif()

set(LVGL_VERSION 9.6.0)
set(LVGL_SHA256 3cbb7ddbedeb1672ad04908afe8ae290561d625d9307ea6a875d3863dae348ae)
set(LVGL_ROOT "${CMAKE_CURRENT_BINARY_DIR}/lvgl-${LVGL_VERSION}")

if(NOT EXISTS "${LVGL_ROOT}/lvgl.h")
    set(_lvgl_archive "${CMAKE_CURRENT_BINARY_DIR}/lvgl-${LVGL_VERSION}-lite.zip")
    set(_lvgl_extract "${CMAKE_CURRENT_BINARY_DIR}/lvgl-extract")
    set(_lvgl_top "lvgl-v${LVGL_VERSION}-lite")
    message(STATUS "Downloading LVGL ${LVGL_VERSION}")
    file(DOWNLOAD
        "https://github.com/lvgl/lvgl/releases/download/v${LVGL_VERSION}/lvgl-v${LVGL_VERSION}-lite.zip"
        "${_lvgl_archive}"
        EXPECTED_HASH SHA256=${LVGL_SHA256}
        STATUS _lvgl_status)
    list(GET _lvgl_status 0 _lvgl_status_code)
    if(NOT _lvgl_status_code EQUAL 0)
        list(GET _lvgl_status 1 _lvgl_status_message)
        message(FATAL_ERROR "LVGL download failed: ${_lvgl_status_message}")
    endif()
    file(REMOVE_RECURSE "${_lvgl_extract}")
    file(ARCHIVE_EXTRACT
        INPUT "${_lvgl_archive}"
        DESTINATION "${_lvgl_extract}"
        PATTERNS
            "${_lvgl_top}/include"
            "${_lvgl_top}/src"
            "${_lvgl_top}/lvgl.h"
            "${_lvgl_top}/lvgl_private.h"
            "${_lvgl_top}/LICENCE.txt")
    file(RENAME "${_lvgl_extract}/${_lvgl_top}" "${LVGL_ROOT}")
    file(REMOVE_RECURSE "${_lvgl_extract}")
    file(REMOVE "${_lvgl_archive}")
endif()

set(LVGL_SRC_DIRS
    .
    core
    debugging/sysmon
    display
    draw
    draw/convert
    draw/snapshot
    draw/sw
    draw/sw/blend
    font
    font/fmt_txt
    font/imgfont
    indev
    layouts
    layouts/flex
    layouts/grid
    libs/qrcode
    logging
    misc
    misc/cache
    misc/cache/class
    misc/cache/instance
    osal
    others/fragment
    others/translation
    stdlib
    stdlib/clib
    themes
    themes/default
    themes/mono
    themes/simple
    tick
    widgets/animimage
    widgets/arc
    widgets/arclabel
    widgets/bar
    widgets/button
    widgets/buttonmatrix
    widgets/calendar
    widgets/canvas
    widgets/chart
    widgets/checkbox
    widgets/dropdown
    widgets/image
    widgets/imagebutton
    widgets/ime
    widgets/keyboard
    widgets/label
    widgets/led
    widgets/line
    widgets/list
    widgets/menu
    widgets/msgbox
    widgets/property
    widgets/qrcode
    widgets/roller
    widgets/scale
    widgets/slider
    widgets/span
    widgets/spinbox
    widgets/spinner
    widgets/switch
    widgets/table
    widgets/tabview
    widgets/textarea
    widgets/tileview
    widgets/win)

set(LVGL_SRCS
    "${LVGL_ROOT}/src/fs/lv_fs.c"
    "${LVGL_ROOT}/src/image/lv_bin_decoder.c"
    "${LVGL_ROOT}/src/image/lv_image_decoder.c")
foreach(_lvgl_dir ${LVGL_SRC_DIRS})
    file(GLOB _lvgl_dir_srcs "${LVGL_ROOT}/src/${_lvgl_dir}/*.c")
    list(APPEND LVGL_SRCS ${_lvgl_dir_srcs})
endforeach()

if(CONFIG_LV_MALLOC_SPIRAM)
    list(APPEND LVGL_SRCS "lv_mem_spiram.c")
endif()

idf_component_register(
    SRCS ${LVGL_SRCS}
    INCLUDE_DIRS "${LVGL_ROOT}")

if(ESP_PLATFORM)
    # FreeRTOS's atomic.h defines its helpers static without inline.
    set_source_files_properties("${LVGL_ROOT}/src/osal/lv_freertos.c"
        PROPERTIES COMPILE_OPTIONS "-Wno-unused-function")

    if(CONFIG_LV_ATTRIBUTE_FAST_MEM_USE_IRAM)
        target_compile_definitions(${COMPONENT_LIB} PUBLIC "LV_ATTRIBUTE_FAST_MEM=IRAM_ATTR")
    endif()
else()
    target_compile_definitions(${COMPONENT_LIB} PUBLIC
        "LV_CONF_KCONFIG_EXTERNAL_INCLUDE=\"sdkconfig.h\"")
endif()
