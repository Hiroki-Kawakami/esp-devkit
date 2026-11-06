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
- A cancel takes effect at a packet boundary: a transfer already under way
  completes as `Canceled`, and a packet the device sent before the halt is
  lost with it.

## Controller

On the device the DWC2 controller runs in scatter/gather DMA mode through
IDF's HAL, one channel per pipe. Its interrupt completes transfers directly:
there is no event task. A bulk transfer is at most 64 KB. The CPU writes
transfer descriptors and setup packets through the non-cacheable alias of
internal RAM, so it never shares a cache line with the controller's
write-back; data buffers are written back before an OUT and invalidated
around an IN. Dropping an `Interface` whose transfer is being canceled waits
up to 200 ms for its channel to halt.

## Simulator

The controller is a USB/IP client on connections the devkit host hands over,
from a USB/IP server or from a device plugged into the computer; see
docs/simulator.md. `firmware/usb_host_rs_check` exercises the stack
against `firmware/usbd_loopback`, a vendor bulk echo device:

```sh
(cd firmware/usbd_loopback && devkit run simulator)
(cd firmware/usb_host_rs_check && devkit run simulator)
devkit cmd -t <check host> "usb attach tcp 127.0.0.1:3240" loop queue ctrl cancel
```

On a Tab5, the same pair runs on two boards joined by their USB-A ports
with USB5V off on both.
