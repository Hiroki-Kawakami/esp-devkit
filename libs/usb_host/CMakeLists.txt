set(USB_HOST_SRCS "")

if(ESP_PLATFORM)
    if(CONFIG_USBH)
        list(APPEND USB_HOST_SRCS "src/core/host.cpp" "src/core/transfer.cpp")
    endif()
    if(CONFIG_USBH_MSC)
        list(APPEND USB_HOST_SRCS "src/msc/msc_bot_device.cpp" "src/msc/msc.cpp")
    endif()
    idf_component_register(
        SRCS ${USB_HOST_SRCS}
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "src/core"
        PRIV_REQUIRES esp_blockdev esp_mm fatfs)
else()
    if(CONFIG_USBH)
        list(APPEND USB_HOST_SRCS "src/sim/host_sim.cpp")
    endif()
    if(CONFIG_USBH_MSC)
        list(APPEND USB_HOST_SRCS "src/sim/msc_sim.cpp")
    endif()
    idf_component_register(
        SRCS ${USB_HOST_SRCS}
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "src/core"
        PRIV_REQUIRES harness)
endif()
