# usb_host

USB host class drivers on top of the ESP-IDF host library (`espressif/usb`).
One IDF client and one worker task are shared by every class, so adding a class
does not add tasks.

## Using it

Enable the classes in Kconfig (`CONFIG_USBH_MSC`, `CONFIG_USBH_UAC`); the
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
| `src/msc/msc_bot_device.cpp` | Bulk-Only Transport + SCSI, exposed as an `esp_blockdev` |
| `src/msc/msc.cpp` | connected-device list and the FAT mount table |
| `src/uac/uac_descriptors.cpp` | UAC1 descriptor walk: playback alternates, the feature unit on their path |
| `src/uac/uac_stream.cpp` | isochronous OUT transfers fed from a ring, packet sizing |
| `src/uac/uac_device.cpp` | alternate selection, sampling rate, feature unit requests |
| `src/uac/uac.cpp` | connected-device list |
| `src/sim/` | simulator stand-ins |

Device ownership stays with the class driver: the core passes each new device
address to every enabled class, and a class that finds no interface of its own
returns without keeping it open.

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

Audio devices declare many alternates, and their configuration descriptors
routinely exceed the host library's default
`CONFIG_USB_HOST_CONTROL_TRANSFER_MAX_SIZE` of 256: enumeration then fails in
`ENUM` with "Configuration descriptor larger than control transfer max length".

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
