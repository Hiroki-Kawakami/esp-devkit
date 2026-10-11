# usb_device

A USB device stack for the DWC2 (Synopsys DesignWare OTG) controller, driving
its registers directly in descriptor DMA mode. Functions are composed into one
configuration; each `Device` owns one controller and one task, so the
high-speed and full-speed controllers can run side by side. Only the
high-speed controller is implemented so far.

## Using it

Enable `CONFIG_USBD`, then:

```cpp
usb_device::DeviceConfig config;
config.info.vendor_id = ...;
config.info.product_id = ...;
auto device = std::make_unique<usb_device::Device>(std::move(config));
device->add(function);
device->start();
```

A `Function` declares its interfaces and endpoints through `ConfigBuilder` in
`describe()`. The builder numbers interfaces, endpoints and strings, fills in
lengths and counts, and puts an interface association in front of a function
with more than one interface. A high-speed port builds the configuration for
both speeds, for the device qualifier and the other-speed configuration.

Endpoints are opened when the host selects the configuration or an alternate
setting, and `configured()` runs after that. Class and vendor requests reach
`Function::control()`: those to an interface or endpoint go to its function,
those to the device to each function in turn. The data stage of a control
write is received before the call.

The API is C++ in `namespace usb_device`.

## Transfers

`Endpoint::submit()` queues a `Transfer` that points at the caller's buffer;
the controller moves data to and from it directly, and completion runs the
callback on the device task. An OUT buffer is invalidated on submit and on
completion, so it must start and end on a cache line (the larger of internal
RAM's and PSRAM's) and be a multiple of the max packet size; an IN buffer is
written back on submit and needs only 4-byte alignment. PSRAM works for both:
when it cannot keep up the controller NAKs, and nothing is lost.

One transfer runs at a time per endpoint. It is spread over up to 16
descriptors of 65024 bytes at high speed, so a transfer of up to about 1 MB
takes one interrupt per descriptor for OUT and one in total for IN. A short
packet ends an OUT transfer in whichever descriptor it lands; the controller
disables the endpoint there and does not touch the next descriptors, so the
transfer boundary holds.

An isochronous transfer is one packet instead, and up to 16 of them sit in a
ring of descriptors so every service interval has a buffer; more wait in the
queue and move into the ring as packets complete. An OUT packet with no buffer
in the ring is lost. IN descriptors carry the (micro)frame they go out in, so
an isochronous IN starts at the first NAK the host is answered with, which
tells the frames it polls in: the NAK interrupt is unmasked only while one
waits for that, since every idle bulk IN NAKs too. A ring that runs dry stops
the endpoint; it starts over the same way, OUT at once.

Measured on the high-speed controller to a macOS host through a hub: bulk OUT
at about 41 MB/s, the same into PSRAM as into internal RAM, so the bus and not
the device sets the pace.

## MS OS 2.0

Off by default. With `DeviceInfo::ms_os_20` the device reports bcdUSB 2.1 and
answers GET_DESCRIPTOR(BOS) with a USB 2.0 extension (no LPM) and the MS OS
2.0 platform capability, and the vendor request it names
(`ms_os_20_vendor_code`, wIndex 7) with the descriptor set. The set holds the
WinUSB compatible ID, and a DeviceInterfaceGUIDs registry property when a GUID
is given, for each function that called `ConfigBuilder::winusb()`. A device
with one interface gets them at the top level; a composite device gets a
function subset per function, so the others keep their class drivers.

## Vendor

`Vendor` (`CONFIG_USBD_VENDOR`) is one vendor-specific interface with a bulk
OUT and/or IN endpoint, and hands its configuration changes and vendor
requests to callbacks.

## UAC

`UacSpeaker` (`CONFIG_USBD_UAC`) is a USB Audio Class 2.0 speaker: an Audio
Control interface with a clock source, a feature unit for mute and volume and
a speaker terminal, and an Audio Streaming interface whose alternates from 1
on are the configured formats. The sample rates, volume range and endpoint
intervals are configuration too.

- **Async** plays on the device's clock: a feedback endpoint reports the rate
  given to `set_feedback()`, in 16.16 samples per microframe at high speed and
  10.14 per frame at full speed. The caller derives it from how full its
  buffer is.
- **Adaptive** has no feedback endpoint; the caller follows the host's rate.
- The data endpoint's packets hold one sample frame more than the rate needs,
  for the clocks to drift into.
- Rate, mute and volume requests are answered from the configuration, and
  changes reach the caller through callbacks on the device task, as do the
  stream starting and stopping and each received packet.

## Layout

| path | what it is |
|---|---|
| `src/core/device.cpp` | the device task, standard requests, dispatch to functions |
| `src/core/descriptors.cpp` | `ConfigBuilder`, device/configuration/string descriptors |
| `src/core/dcd.hpp` | the controller interface the core is written against |
| `src/dwc2/dcd.cpp` | the DWC2 controller |
| `src/vendor/vendor.cpp` | the vendor-specific function |
| `src/uac/uac.cpp` | the USB Audio Class 2.0 speaker |
| `src/sim/dcd.cpp` | the simulator's controller, a USB/IP server |

The core never touches hardware: the controller posts bus events and SETUP
packets to the device task's queue, and the task answers through `Dcd`. Which
`Dcd` is built is chosen by CMake, so the simulator runs the same core.

## Simulator

The simulator's controller exports the device over USB/IP, on
`SIMULATOR_USBD_LISTEN` (`<ipv4>:<port>`, default `127.0.0.1:3240`) with bus
id `1-1`. Any USB/IP client attaches to it: `usbip attach -r <host> -b 1-1`
on Linux, or a client of its own over TCP. An import is the bus reset and
enumeration; the client then sends SET_CONFIGURATION itself, which the
Linux one does as part of attaching. Each attached session sees the device
configured afresh, and closing the connection is the device's disconnect.

A host URB is a run of packets. One whose length is not a multiple of the
max packet size, a zero-length one, or one flagged for a trailing
zero-length packet ends the OUT transfer it lands in, as a short packet does
on the bus; a URB that finds no transfer queued waits, as a NAK would. IN
URBs collect transfers the same way. Each packet of an isochronous URB is one
device transfer, completed at once: an OUT packet with no transfer queued is
lost and an IN one goes back empty, and IN data goes back packed, as the Linux
USB/IP server sends it.

## Controller

There is no VBUS sensing: B-valid is forced high, so a cable pull is not
reported. On a board whose port can also supply VBUS, that supply has to stay
off while the port is a device.

The FIFO RAM above `GDFIFOCFG.EPInfoBaseAddr` holds the controller's
per-endpoint DMA state, so the RX and TX FIFOs are fitted below it. The base is
read from the register as reset left it.

Control transfers follow the descriptor DMA flow: EP0 OUT is armed with one
64-byte descriptor that receives the SETUP and also the status OUT, and the
SETUP is read on the setup-phase-done interrupt, not on the transfer completion
before it. A SETUP that lands in the status OUT descriptor is recognised by the
descriptor's SR bit. The status IN of a control write is armed only once the
host has started the status stage (StsPhseRcvd).

Disabling an endpoint with a transfer on it follows the controller's
sequence: IN NAK effective then disable for IN, global OUT NAK then disable
for OUT. Each IN endpoint gets its own TX FIFO of two packets, allocated on
first open and kept until the next bus reset.
