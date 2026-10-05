# usb_host

USB host class drivers on top of the ESP-IDF host library (`espressif/usb`).
One IDF client and one worker task are shared by every class, so adding a class
does not add tasks.

## Using it

Enable the classes in Kconfig (`CONFIG_USBH_MSC`, `CONFIG_USBH_UAC`,
`CONFIG_USBH_UVC`); the
component compiles nothing and pulls in no managed dependency when none is
enabled. Then:

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
the worker task, which is also where devices are opened; a callback that blocks
holds up enumeration.

The API is C++ in `namespace usb_host`: the IDF host library's internal layer
already exports the C `usbh_*` namespace, and `usb_host_*` is its public one.

## Layout

| path | what it is |
|---|---|
| `src/core/host.cpp` | host install, client, worker, dispatch to the enabled classes |
| `src/core/transfer.cpp` | blocking bulk/control transfers, halt recovery, buffer lending |
| `src/core/in_stream.cpp` | isochronous or bulk IN transfers kept queued, handed to a sink on the client task |
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
returns without keeping it open. The host library lets a client open a device
only once, so classes open it through the core (`open_device()` /
`close_device()`), which counts users: a camera with a microphone is held by
the video and the audio class at the same time.

## Transfers into the caller's buffer

`TransferContext::set_buffer()` points a transfer at the caller's buffer so the
DMA writes it directly instead of going through the host stack's own buffer:
`urb_alloc()` only assigns `data_buffer`/`data_buffer_size` after allocating
them separately, and `hcd_dwc.c`'s `cache_sync_data_buffer()` documents that
class drivers may overwrite those fields. An IN transfer is synced with
`ESP_CACHE_MSYNC_FLAG_DIR_M2C`, which has no unaligned path, so a borrowed
buffer must be cache aligned in both address and size, and `num_bytes` must be
a multiple of the endpoint's max packet size (`borrowable()`). With
`CONFIG_USB_HOST_DWC_DMA_CAP_MEMORY_IN_PSRAM` a PSRAM buffer is reached behind
the cache, so the class has to call `sync_for_device()` before and
`sync_for_cpu()` after.

Transfers complete on the client task. Anything that waits for one — opening a
device included — must run elsewhere, which is why opens happen on the worker
task rather than in the client event callback.

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

Audio and video devices declare many alternates and frame sizes, and their
configuration descriptors routinely exceed the host library's default
`CONFIG_USB_HOST_CONTROL_TRANSFER_MAX_SIZE` of 256: enumeration then fails in
`ENUM` with "Configuration descriptor larger than control transfer max length".

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

Isochronous IN transfers (video and audio capture) receive into internal RAM
even with `CONFIG_USB_HOST_DWC_DMA_CAP_MEMORY_IN_PSRAM`. With the host stack's
PSRAM buffers, a board that decodes and scans out video at the same time
leaves the controller too little PSRAM bandwidth: packets come back
`SKIPPED`, or the next transfer misses its start and waits a whole 64-entry
frame list (8 ms at high speed), and every frame arrives with holes. Video
transfers are 1 ms (eight packets) so the four in flight take 25 KB of
internal RAM at 800-byte packets.

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
