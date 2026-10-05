set(USB_HOST_SRCS "")

if(ESP_PLATFORM)
    if(CONFIG_USBH)
        list(APPEND USB_HOST_SRCS "src/core/hcd.cpp" "src/core/host.cpp" "src/core/transfer.cpp"
             "src/core/in_stream.cpp")
    endif()
    if(CONFIG_USBH_MSC)
        list(APPEND USB_HOST_SRCS "src/msc/msc_bot_device.cpp" "src/msc/msc.cpp")
    endif()
    if(CONFIG_USBH_UAC)
        list(APPEND USB_HOST_SRCS "src/uac/uac_format.cpp" "src/uac/uac_descriptors.cpp"
             "src/uac/uac_stream.cpp" "src/uac/uac_device.cpp" "src/uac/uac_capture.cpp"
             "src/uac/uac.cpp")
    endif()
    if(CONFIG_USBH_UVC)
        list(APPEND USB_HOST_SRCS "src/uvc/uvc_frames.cpp" "src/uvc/uvc_descriptors.cpp"
             "src/uvc/uvc_device.cpp" "src/uvc/uvc.cpp")
    endif()
    idf_component_register(
        SRCS ${USB_HOST_SRCS}
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "src/core"
        PRIV_REQUIRES esp_blockdev esp_hal_usb esp_hw_support esp_mm esp_timer fatfs hal)
else()
    if(CONFIG_USBH)
        list(APPEND USB_HOST_SRCS "src/sim/host_sim.cpp")
    endif()
    if(CONFIG_USBH_MSC)
        list(APPEND USB_HOST_SRCS "src/sim/msc_sim.cpp")
    endif()
    if(CONFIG_USBH_UAC)
        list(APPEND USB_HOST_SRCS "src/uac/uac_format.cpp" "src/sim/uac_sim.cpp")
    endif()
    if(CONFIG_USBH_UVC)
        list(APPEND USB_HOST_SRCS "src/uvc/uvc_frames.cpp" "src/sim/uvc_sim.cpp")
    endif()
    idf_component_register(
        SRCS ${USB_HOST_SRCS}
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "src/core" "src/uvc"
        PRIV_REQUIRES harness)
endif()
