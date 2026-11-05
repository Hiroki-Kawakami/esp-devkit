# Vendor bulk echo device, the peer of firmware/usb_host_rs_check.
idf_component_register(
    SRCS "usbd_loopback.cpp"
    INCLUDE_DIRS "."
    PRIV_REQUIRES usb_device)
