# usb_host

A USB host stack for the DWC2 (Synopsys DesignWare OTG) controller, written on
ESP-IDF's register-level HAL (`hal/usb_dwc_hal.h`) and PHY driver, with class
drivers on top. Two tasks are shared by every class, so adding a class does not
add tasks. It supports one device on the root port; hubs and interrupt
endpoints are not supported.

## Using it

Enable the classes in Kconfig (`CONFIG_USBH_MSC`, `CONFIG_USBH_UAC`,
`CONFIG_USBH_UVC`); the component compiles nothing when none is enabled.
Then:

```cpp
bsp_power_set_switch(BSP_POWER_SWITCH_USB5V, true);
usb_host::Callbacks callbacks;
callbacks.msc_connected = [](std::shared_ptr<usb_host::MscDevice> device) { ... };
callbacks.msc_disconnected = [](const std::shared_ptr<usb_host::MscDevice>& device) { ... };
usb_host::install(std::move(callbacks));
```

VBUS is left to the caller so it can choose between always-on and powering the
port only while USB is in use. Classes are registered inside `install()`, so a
device that enumerates right after it cannot be missed. The callbacks run on
the worker task, which is also where devices are enumerated and opened; a
callback that blocks holds up enumeration.

The API is C++ in `namespace usb_host`.

## Layout

| path | what it is |
|---|---|
| `src/core/hcd.cpp` | the controller: port, channels, transfer descriptors, interrupt |
| `src/core/host.cpp` | tasks, enumeration, devices and their endpoints, dispatch to the enabled classes |
| `src/core/transfer.cpp` | blocking bulk/control transfers, halt recovery, buffer lending |
| `src/core/in_stream.cpp` | isochronous or bulk IN transfers kept queued, handed to a sink on the event task |
| `src/msc/msc_bot_device.cpp` | Bulk-Only Transport + SCSI, exposed as an `esp_blockdev` |
| `src/msc/msc.cpp` | connected-device list and the FAT mount table |
| `src/uac/uac_descriptors.cpp` | UAC1 descriptor walk: playback or capture alternates, the feature unit on the playback path |
| `src/uac/uac_stream.cpp` | isochronous OUT transfers fed from a ring, packet sizing |
| `src/uac/uac_device.cpp` | alternate selection, sampling rate, feature unit requests |
| `src/uac/uac_capture.cpp` | capture alternates into a ring, per-device format quirks |
| `src/uac/uac.cpp` | connected-device lists |
| `src/uvc/uvc_descriptors.cpp` | UVC descriptor walk: MJPEG frame sizes, isochronous alternates, bulk endpoint |
| `src/uvc/uvc_device.cpp` | probe/commit, alternate selection, payloads into frames |
| `src/uvc/uvc_frames.cpp` | the caller's frame slots: filling, newest-ready, held |
| `src/uvc/uvc.cpp` | connected-device list |
| `src/sim/` | simulator stand-ins |

Device ownership stays with the class driver: the core passes each new device
address to every enabled class, and a class that finds no interface of its own
returns without keeping it open. Classes open a device through the core
(`open_device()` / `close_device()`), which counts users: a camera with a
microphone is held by the video and the audio class at the same time.

## Host controller

`Hcd` drives the controller in scatter/gather DMA mode through IDF's HAL. Its
interrupt only moves descriptors and queues what finished; an event task runs
the completion callbacks, and a worker task handles connection, enumeration
and disconnection. Every transfer completes on the event task, so anything that
waits for one — enumeration and opening a device included — runs elsewhere.

Enumeration reads the first 8 bytes of the device descriptor at address 0,
sends SET_ADDRESS and reads the rest. espressif/usb resets the bus a second
time between the first read and SET_ADDRESS; some devices then never answer at
their new address, so the first attempt goes without it, the second keeps it
for devices that need it, and a third goes without it again.

Control and bulk transfers run one at a time per endpoint, each started from
the interrupt that finished the previous one. An isochronous endpoint instead
keeps its channel running over a ring of descriptors, one per (micro)frame: 256
at high speed and 64 at full speed, which is what `isoc_slots()` hands out to
the classes as lookahead. Queued transfers are written ahead of the
controller and reaped behind it, so streaming has no gaps while transfers keep
coming; the channel stops only when nothing is queued. A descriptor whose
(micro)frame passes unserved keeps its active bit, so a transfer counts as done
once the controller's position has passed it, and its unserved packets come
back as skipped. The CPU reads and writes descriptors through the
non-cacheable alias of internal RAM, so it never shares a cache line with the
controller's write-back of a neighbouring descriptor. IDF's HAL addresses only
64 descriptors, so the high-speed ring is started by the HCD itself.

Data buffers are synced around every transfer by the host stack. An IN buffer
is invalidated whole, which is why a borrowed one must start and end on a
cache line.

## Transfers into the caller's buffer

`transfer_set_buffer()` points a transfer at the caller's buffer so the DMA
writes it directly, and `TransferContext::borrowable()` says whether a buffer
qualifies: cache aligned in address and size, and a multiple of the endpoint's
max packet size. A PSRAM buffer is fine for bulk, which is how mass storage
reads land in the caller's buffer.

## Mass storage

`MscDevice` is a block device: `read()`/`write()` work on it directly, with no
filesystem involved. `msc_bot_device.cpp` implements Bulk-Only Transport and
the SCSI subset a drive needs (INQUIRY, TEST UNIT READY, REQUEST SENSE, READ
CAPACITY(10), READ10/WRITE10). Every drive that enumerates gets its own
`MscDevice`.

`mount()` puts FAT on a device through IDF's `esp_vfs_fat_bdl_mount()`, never
formatting it, and `unmount()` takes the mount point. `ff_bdl_read()` hands
FatFS's buffer straight to the block device, so a large aligned read still
lands in the caller's buffer; anything else (FatFS's own window buffer) goes
through a 4 KB bounce buffer. CBW and CSW ride a separate 512-byte transfer.

The device object lives as long as anyone holds it: the class driver while the
drive is connected, the mount table while it is mounted, and the application.
Pulling a mounted drive leaves the mount registered — FAT must not be
unregistered under an open fd — with I/O failing and `mounted()` reporting
false until `unmount()`. The last reference releases the interface and closes
the USB device.

`usb_host_msc` is not used because it copies every data phase through its own
transfer buffer, which with PSRAM transfer buffers touches PSRAM three times
per read. Its single buffer also grows to the largest read FAT asks for, which
makes every 31-byte command sync that whole cache range, and a failed regrow
leaves a dangling pointer that the next command frees again (1.3.0).

## Audio

`UacDevice` plays PCM on a USB Audio Class 1.0 device: Type I PCM alternates of
the first streaming interface that has an isochronous OUT endpoint. Only
adaptive and synchronous endpoints are taken, where the device follows the
host's SOF and the host decides each packet's size; an asynchronous endpoint
needs its feedback endpoint read and is skipped with a warning. A device left
with no usable alternate is not reported at all.

`formats()` lists what the device declared and `open()` takes one of them and
a rate; the driver neither converts nor resamples. Three transfers of about
8 ms each stay queued, refilled from a 40 ms ring in their completion callback
on the client task, so streaming adds no task. A ring that runs dry sends
silence and the stream keeps its clock; `write()` blocks while the ring is
full, which paces the caller like an I2S DMA queue does.

Volume and mute go to the feature unit on the path from the streaming
interface's terminal to an output terminal, through the master channel when it
has the control and every logical channel otherwise. The range in
`volume_min_db()`/`volume_max_db()` is what the device answers to GET_MIN and
GET_MAX at connect; UAC1 descriptors do not carry it. Those requests are sent
from the client task one after another, holding only the latest value, so
`set_volume_db()` never waits on the bus.

`UacCaptureDevice` records from the first streaming interface with an
isochronous IN endpoint, in any synchronization mode: the device sends what it
has each (micro)frame and the host only reads. Packets land in a 200 ms ring
from the completion callback; `read()` waits for at least one frame, and what
arrives while the ring is full is dropped. `available()` is the ring's fill,
which is the only measure of how the device's clock runs against the
consumer's. A device exposing both directions is reported once as a
`UacDevice` and once as a `UacCaptureDevice`.

`formats()` of a capture device is what it sends. A few devices declare one
format and send another at the same byte rate; `kQuirks` in `uac_capture.cpp`
rewrites those formats by vendor and product, and `open()` still asks the
device for the rate it declared.

Audio and video devices declare many alternates and frame sizes, so their
configuration descriptors run to kilobytes; enumeration reads up to 4 KB.

## Video

`UvcDevice` streams MJPEG from the first video streaming interface that offers
it; uncompressed and frame-based formats are ignored. `frame_sizes()` lists the
MJPEG frame descriptors with their intervals in 100 ns units. `start()` runs
PROBE/COMMIT for one size and interval and then streams.

Isochronous devices get the alternate with the smallest packet that holds the
committed `dwMaxPayloadTransferSize`, or the largest one when none does.
Alternates with more than one transaction per microframe are skipped: the host
stack schedules one transaction per (micro)frame, so 1024 bytes per microframe
(8 MB/s) is the ceiling. Bulk devices stream on alternate 0; a payload ends
with a short transfer or once it reaches `dwMaxPayloadTransferSize`, and only
its first bytes carry a header. Either way the transfers stay queued like UAC
playback's, so streaming adds no task.

IN streams (video, and audio capture) receive into internal RAM. While other
masters keep PSRAM busy, the controller cannot drain its RX FIFO into it in
time, and it then misses the (micro)frames of every isochronous endpoint on the
bus: with a bulk camera's buffers in PSRAM and the picture being decoded, its
microphone lost one packet in twelve. Isochronous video transfers are 1 ms
(eight packets), so four in flight take 25 KB at 800-byte packets; a bulk
camera's four transfers take its payload size each, 64 KB at 16 KB.

Frames are assembled into slots the caller passes to `start()`, so the driver
allocates no frame memory. A frame ends at the EOF bit or when the frame ID
toggles; one with the error bit, a bus error, no JPEG SOI, or more bytes than a
slot is dropped. `receive()` returns the newest complete frame and a newer one
replaces a frame nobody received yet, so a slow consumer sees latency drop
frames, not grow. A slot is taken on the first data byte rather than the
header, because devices send header-only payloads between frames and taking a
slot for one could recycle the frame that just completed. `stop()` retires the
session: frames still held remain readable, and releasing them afterwards does
nothing.

## Simulator

`install()` registers the harness commands of the enabled classes. For MSC, a
host directory stands in for the drive: `SIMULATOR_USBH_MSC_PATH` is attached
at boot when set, and `usbh-msc-attach [dir]` / `usbh-msc-detach` plug and
pull it. The simulator's `MscDevice` has no blocks; `mount()` maps the mount
point onto the directory through `simulator/path_redirect.h`.

For UAC, `usbh-uac-attach [wav] [rate,...] [novolume]` plugs a device that
offers 16- and 24-bit stereo at the given rates (44100,48000 by default) and
records what it is sent to the WAV file (`captures/usb_audio.wav` by default)
at real-time pace; `usbh-uac-detach` pulls it. Volume and mute only show up in
the log, and `novolume` leaves the device without a volume control.
`usbh-uac-capture-attach [rate]` plugs a capture device that records a 440 Hz
tone in 16-bit stereo at real-time pace (48000 by default);
`usbh-uac-capture-detach` pulls it.

For UVC, `usbh-uvc-attach [path] [WxH]` plugs a camera that streams JPEG files
at the requested interval, offering one size (1280x720 by default) at 30 and
15 fps. `path` is a JPEG file or a directory of them, played in name order and
looped, and defaults to `SIMULATOR_USBH_UVC_PATH`; the files are sent as they
are, so their size should match. `usbh-uvc-detach` pulls it.
