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

The API is C++ in `namespace usb_device`.

## Layout

| path | what it is |
|---|---|
| `src/core/device.cpp` | the device task, standard requests, dispatch to functions |
| `src/core/descriptors.cpp` | `ConfigBuilder`, device/configuration/string descriptors |
| `src/core/dcd.hpp` | the controller interface the core is written against |
| `src/dwc2/dcd.cpp` | the DWC2 controller |
| `src/sim/dcd.cpp` | the simulator's controller, which fails to start |

The core never touches hardware: the controller posts bus events and SETUP
packets to the device task's queue, and the task answers through `Dcd`. Which
`Dcd` is built is chosen by CMake, so the simulator runs the same core.

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
descriptor's SR bit.
