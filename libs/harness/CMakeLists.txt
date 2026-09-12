# esp-devkit harness — scripted UI verification over a line protocol.
#
# One command set on both targets (see inc/harness.h): synthetic touch/button
# input, an idle probe, and a JPEG capture of the panel, all through the
# bsp_harness.h hooks. What differs is the transport: the device answers on
# the IDF log console (UART / USB-Serial-JTAG), the simulator on stdin/stdout.
# tools/harness/harness.py speaks the protocol and runs the same script
# against either. CONFIG_HARNESS gates the whole thing and selects the
# bsp_harness.h hooks it drives; device builds compile nothing without it.
set(HARNESS_COMMON_SRCS
    "src/harness.c"
    "src/harness_cmds.c"
    "src/harness_capture.c")

if(ESP_PLATFORM)
    set(HARNESS_SRCS "")
    if(CONFIG_HARNESS)
        set(HARNESS_SRCS ${HARNESS_COMMON_SRCS} "src/harness_console.c")
    endif()
    idf_component_register(
        SRCS ${HARNESS_SRCS}
        INCLUDE_DIRS "inc"
        REQUIRES bsp image_framework
        PRIV_REQUIRES esp_driver_uart esp_driver_usb_serial_jtag vfs)
else()
    idf_component_register(
        SRCS ${HARNESS_COMMON_SRCS} "src/harness_stdio.c"
        INCLUDE_DIRS "inc")
endif()
