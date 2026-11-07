# usb_host_rs

An async USB host stack in Rust. It enumerates what is plugged in and lists
it; whoever wants a device claims its interfaces when they like. One device on
the root port; no hubs. Control and bulk transfers so far.

## Using it

```rust
usb_host_rs::start(&usb_host_rs::Config::default())?;

let mut watch = usb_host_rs::watch();
loop {
    match watch.next().await {
        Event::Arrived(device) => { /* claim an interface now, or keep the device for later */ }
        Event::Left(device) => { /* forget it */ }
    }
}
```

- `devices()` lists what is plugged in now. `watch()` first reports each of
  those as `Arrived`, then every arrival and departure, so nothing falls
  between the two. Any number of watches can run, each seeing every event;
  undelivered events wait in it.
- `Device::id()` names one enumeration: plugging the same device in again
  gives a new one.
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
- `transfer_at` moves part of a buffer from an offset on a cache line
  (`Buffer::ALIGN`), so one buffer can collect more than one transfer.
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
