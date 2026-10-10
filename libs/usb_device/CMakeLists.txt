set(USB_DEVICE_SRCS "")

if(CONFIG_USBD)
    list(APPEND USB_DEVICE_SRCS "src/core/device.cpp" "src/core/descriptors.cpp")
    if(ESP_PLATFORM)
        list(APPEND USB_DEVICE_SRCS "src/dwc2/dcd.cpp")
    else()
        list(APPEND USB_DEVICE_SRCS "src/sim/dcd.cpp")
    endif()
endif()

if(ESP_PLATFORM)
    idf_component_register(
        SRCS ${USB_DEVICE_SRCS}
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "src/core"
        PRIV_REQUIRES esp_hal_usb esp_hw_support esp_mm hal soc)
else()
    idf_component_register(
        SRCS ${USB_DEVICE_SRCS}
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "src/core")
endif()
