# Shared epd_tweak app component. Picks up new .c/.cpp files on cmake re-run
# (CONFIGURE_DEPENDS isn't usable here -- IDF evaluates this file in script mode
# for requirement extraction, where it's invalid).
file(GLOB_RECURSE APP_SRCS
     "${CMAKE_CURRENT_SOURCE_DIR}/*.c"
     "${CMAKE_CURRENT_SOURCE_DIR}/*.cpp")

idf_component_register(
    SRCS ${APP_SRCS}
    INCLUDE_DIRS "."
    REQUIRES bsp ui_framework image_framework
    PRIV_REQUIRES esp_driver_usb_serial_jtag esp_driver_uart)

# paper_s3 only: reach into the bsp's private epd_ll headers for the
# waveform-tuning backdoor (see epd_ll_tweak.h). The esp32/paper build has no
# epd_ll, so SerialCmd.cpp compiles the wf/dump commands out.
if(EPD_LL_TWEAK)
    target_compile_definitions(${COMPONENT_LIB} PRIVATE EPD_LL_TWEAK=1)
    target_include_directories(${COMPONENT_LIB} PRIVATE
        "${CMAKE_CURRENT_SOURCE_DIR}/../../../bsp/driver/epd"
        "${CMAKE_CURRENT_SOURCE_DIR}/../../../bsp/inc_private")
endif()
