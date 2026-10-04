# usb_host

USB host class drivers on top of the ESP-IDF host library (`espressif/usb`).
One IDF client and one worker task are shared by every class, so adding a class
does not add tasks.

## Using it

Enable the classes in Kconfig (`CONFIG_USBH_MSC`); the component compiles
nothing and pulls in no managed dependency when none is enabled. Then:

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

## Simulator

`install()` registers the harness commands of the enabled classes. For MSC, a
host directory stands in for the drive: `SIMULATOR_USBH_MSC_PATH` is attached
at boot when set, and `usbh-msc-attach [dir]` / `usbh-msc-detach` plug and
pull it. The simulator's `MscDevice` has no blocks; `mount()` maps the mount
point onto the directory through `simulator/path_redirect.h`.
