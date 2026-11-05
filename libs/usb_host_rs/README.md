# usb_host_rs

An async USB host stack in Rust. Class drivers, built in or the application's
own, are handed every device that enumerates. One device on the root port; no
hubs. Control and bulk transfers so far.

## Using it

```rust
struct MyDriver;

impl usb_host_rs::Driver for MyDriver {
    fn attached(&self, device: Device) {
        // claim an interface, keep it, spawn the work on an executor
    }
}

static DRIVERS: [&dyn usb_host_rs::Driver; 1] = [&MyDriver];
usb_host_rs::start(&usb_host_rs::Config::new(&DRIVERS))?;
```

- `attached` runs on the host's own task, where enumeration runs too, so it
  must not block. A driver that finds nothing of its own drops the `Device`.
- `Device` is a shared handle. Transfers fail with `Error::NoDevice` once it
  is unplugged and `disconnected()` completes then; the last handle dropped
  releases it.
- `claim(interface, alternate)` opens the alternate's bulk endpoints without
  sending SET_INTERFACE; the interface is released once the `Interface` and
  its `Endpoint`s are dropped. Another claim of it fails with `Busy`.
- Transfers take a `Buffer` and give it back with the result. Several may be
  queued on one endpoint and run in order. Dropping the future cancels the
  transfer; the buffer is freed once the controller lets go of it. There are
  no timeouts: wrap the future in `idf::executor::timeout`.
- VBUS is the caller's.

## Simulator

The controller is a USB/IP client on connections the devkit host hands over;
see docs/simulator.md. `firmware/usb_host_rs_check` exercises the stack
against `firmware/usbd_loopback`, a vendor bulk echo device:

```sh
(cd firmware/usbd_loopback && devkit run simulator)
(cd firmware/usb_host_rs_check && devkit run simulator)
devkit cmd -t <check host> "usb attach tcp 127.0.0.1:3240" loop queue ctrl cancel
```

On the device there is no controller yet: `start` returns `Unsupported`.
