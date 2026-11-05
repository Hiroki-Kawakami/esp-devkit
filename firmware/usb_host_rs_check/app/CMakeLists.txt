# usb_host_rs check app, against firmware/usbd_loopback.
idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES bsp_rs devkit harness_rs idf rust_rt usb_host_rs)
devkit_rust_crate(ROOT "lib.rs")
