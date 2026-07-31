# Wi-Fi station connection manager

`wifi` is the application-facing ESP-IDF component for managing a Wi-Fi
station. It owns radio initialization, scanning, connection state, saved
credentials, automatic reconnection, and radio enable/disable behavior behind
the C++ `wifi::Manager` API.

The same API works with:

- ESP32 targets that provide native Wi-Fi, using ESP-IDF's local `esp_wifi`
  implementation.
- ESP32-P4 targets connected to a Wi-Fi coprocessor, using `esp-hosted` and
  `esp_wifi_remote`.
- The desktop simulator, using a deterministic and scriptable fake backend.

## Adding the component to firmware

`devkit.cmake` always adds this directory to `EXTRA_COMPONENT_DIRS`, making the
component discoverable by every firmware project. A firmware component only
links and uses the API when it declares `wifi` as a dependency:

```cmake
idf_component_register(
    SRCS "app.cpp"
    INCLUDE_DIRS "."
    REQUIRES wifi)
```

Include the public header from C++ code:

```cpp
#include "wifi_manager.hpp"
```

On ESP32-P4, the component manifest adds `esp_hosted` and `esp_wifi_remote`
automatically. The board firmware remains responsible for selecting and
configuring the actual hosted transport, coprocessor target, pins, and related
sdkconfig options. Native Wi-Fi targets do not pull in these remote-only
dependencies.

## Responsibilities

The component owns:

- The Wi-Fi station lifecycle: `esp_netif`, the default event loop, `esp_wifi`,
  and the station netif.
- Scanning, connecting, disconnecting, and reporting link state.
- Persisting the most recently requested SSID and password in NVS.
- Reconnecting to the saved network at startup or when the radio is enabled.
- Enabling and disabling the radio while retaining the initialized stack.
- Translating ESP-IDF disconnect reasons into stable application-level results.

The BSP does not initialize Wi-Fi. It only provides board-specific hosted
transport configuration where required.

## Backend selection

The build selects one implementation without changing the public API:

| Target | Wi-Fi implementation | Station netif |
| --- | --- | --- |
| ESP32, ESP32-C2/C3/C5/C6/C61, ESP32-S2/S3 | Native `esp_wifi` | `esp_netif_create_default_wifi_sta()` |
| ESP32-P4 | `esp_wifi_remote` over `esp-hosted` | Component-owned remote netif setup |
| Desktop simulator | Scriptable fake | Not applicable |

Unsupported device targets fail during CMake configuration instead of silently
building a non-functional backend.

### ESP32-P4 netif workaround

The ESP32-P4 path intentionally does not call
`esp_netif_create_default_wifi_sta()`. Some ESP-Hosted configurations can emit
duplicate station start and stop events while the remote interface is being
initialized. Passing those events directly to the default handlers can start or
stop the same netif more than once.

`src/wifi_netif_remote.cpp` installs equivalent station handlers while tracking
whether the netif is already started. Duplicate start and stop events are
ignored, and the RX callback, netstack buffer callbacks, and station MAC are
registered before the netif is started. This workaround is specific to the
remote backend; native Wi-Fi continues to use ESP-IDF's default station netif.

## Public API

The process-wide manager is returned by `wifi::manager()`:

```cpp
auto& wifi_manager = wifi::manager();
```

The singleton is intentionally allocated for the lifetime of the process so
that FreeRTOS tasks and late callbacks cannot observe a destroyed manager.

### State model

```text
State::Off          Radio is not initialized yet, or has been disabled
State::Disconnected Radio is enabled but no usable connection exists
State::Connecting   Association or IP acquisition is in progress
State::Connected    The station is associated and has obtained an IP address
```

`Status` contains the current state, SSID, IP address, and RSSI. The IP address
and RSSI are meaningful only in `State::Connected`. `mac_address()` is a
separate query because the station MAC belongs to the interface rather than to
a particular connection; it returns an empty string before backend startup.

### Listener and startup

Implement `wifi::Listener` to receive persistent state changes:

```cpp
class NetworkListener : public wifi::Listener {
public:
    void on_wifi_state(const wifi::Status& status) override {
        // Forward status to the application thread if necessary.
    }
};

auto listener = std::make_shared<NetworkListener>();
wifi::manager().start(listener);
```

The manager stores the listener as a `std::weak_ptr`. Retain the corresponding
`std::shared_ptr` for as long as notifications are needed; destroying it detaches
the listener without an explicit unregister call.

`start()` registers the listener, queues backend startup, and attempts to join
the saved network if one exists. It is non-blocking. Use `set_listener()` when a
consumer only needs to replace the listener without starting the radio.

For boot code that has already registered a listener, the same startup and
reconnection behavior is available without changing that listener:

```cpp
wifi::manager().set_listener(listener);
wifi::manager().autoconnect_saved();
```

Radio startup and enable/disable operations are serialized on one manager-owned
FreeRTOS worker task. This prevents overlapping callers from racing Wi-Fi stack
initialization.

### Scan

```cpp
wifi::manager().scan(
    [](wifi::Result result, std::vector<wifi::AP> access_points) {
        if (result != wifi::Result::Ok) {
            return;
        }
        for (const auto& access_point : access_points) {
            // access_point.ssid, access_point.rssi, access_point.secured
        }
    });
```

Scan results omit hidden entries, deduplicate records by SSID, keep the
strongest record for each SSID, and sort the result by descending RSSI. Call
`start()`, `autoconnect_saved()`, or `set_enabled(true)` before scanning.

Only one scan completion is retained. Starting another scan supersedes the
previous request and completes its callback with `Result::Failed` and an empty
list.

### Connect and saved credentials

```cpp
wifi::manager().connect(
    "YOUR_SSID", "YOUR_PASSWORD",
    [](wifi::Result result) {
        // The callback completes once for this connection attempt.
    },
    15000);
```

The credentials are written to the `wifi` NVS namespace when an initialized
backend accepts the connection request. They are then available through:

```cpp
bool has_saved_network = wifi::manager().configured();
std::string ssid = wifi::manager().saved_ssid();

wifi::manager().connect_saved([](wifi::Result result) {
    // Result::Failed means no saved credentials or a generic connection error.
});
```

The connection callback fires exactly once. A later link loss is reported to
the persistent `Listener`, not by invoking the completed connection callback
again. If no terminal event arrives before `timeout_ms`, the manager disconnects
the backend and completes the request with `Result::Timeout`.

`forget()` erases saved credentials without disconnecting an active link:

```cpp
wifi::manager().forget();
```

### Result values

| Result | Meaning |
| --- | --- |
| `Ok` | The requested operation completed successfully |
| `ApNotFound` | The requested access point was not found |
| `AuthFailed` | Authentication or the security handshake failed |
| `AssocFailed` | Association or connection setup failed |
| `IpFailed` | Association succeeded but the link dropped before IP acquisition |
| `Timeout` | No terminal connection event arrived before the deadline |
| `Failed` | Generic failure, missing saved credentials, or a superseded request |

The device backend derives these values from ESP-IDF station disconnect reasons.

### Radio enable and disable

```cpp
wifi::manager().set_enabled(false);

wifi::manager().set_enabled(true, [] {
    // The radio operation has been applied by the worker task.
});

bool radio_enabled = wifi::manager().enabled();
```

`set_enabled()` is non-blocking and idempotent. Enabling the radio initializes
the backend if needed and attempts to reconnect to the saved network. Disabling
it stops the radio, moves the manager to `State::Off`, and fails any pending
connection callback. The optional completion runs after the serialized worker
operation, including when the requested state was already applied.

`disconnect()` leaves the current access point but keeps the radio enabled and
moves the manager to `State::Disconnected`:

```cpp
wifi::manager().disconnect();
```

### Power saving

```cpp
wifi::manager().set_power_save(wifi::PowerSave::Default);
wifi::manager().set_power_save(wifi::PowerSave::None);
```

`Default` selects ESP-IDF minimum-modem power saving. `None` disables Wi-Fi
power saving. The simulator accepts both modes as no-ops.

## Callback execution context

Wi-Fi event completions and persistent listener updates normally run on the
backend event context: the ESP-IDF event loop on device and an asynchronous fake
backend thread in the simulator. The `set_enabled()` completion runs on the
manager worker task. A callback displaced by a newer scan or connection request
can run synchronously in the caller that superseded it.

Callbacks must therefore be thread-safe and must marshal UI updates to the UI
thread. In particular, they must not call LVGL directly unless the application
has already entered the LVGL execution context.

Thread-safe query methods such as `status()`, `configured()`, `saved_ssid()`, and
`enabled()` may be called from other application tasks.

## Simulator backend

The simulator never accesses the host's real Wi-Fi interface. Its deterministic
fake can be controlled through `wifi_sim.hpp`:

```cpp
#include "wifi_sim.hpp"

wifi::sim::set_aps({
    {"YOUR_SSID", -40, true},
    {"YOUR_OPEN_SSID", -65, false},
});
wifi::sim::set_event_delay_ms(10);
wifi::sim::set_next_connect_result(wifi::Result::AuthFailed);
wifi::sim::drop_link();
```

Without an explicitly forced result, an SSID containing one of the following
tokens selects a canned outcome:

| SSID token | Outcome |
| --- | --- |
| `fail-auth` | `AuthFailed` |
| `fail-notfound` | `ApNotFound` |
| `fail-assoc` | `AssocFailed` |
| `timeout` | No backend event; the manager timeout completes the request |
| Anything else | `Ok` |

The `SIMULATOR_WIFI_CONNECT` environment variable can set the next result for a
headless run. Accepted values are the `Result` names listed above.

Calling `wifi::sim::register_harness_commands()` adds these commands to the
simulator harness:

| Command | Purpose |
| --- | --- |
| `wifi-aps` | Replace the fake scan list |
| `wifi-connect-result` | Set the next connection result |
| `wifi-delay` | Set the asynchronous event delay in milliseconds |
| `wifi-drop` | Simulate a link loss |

## Source layout

```text
inc/wifi_manager.hpp        Public manager API
inc/wifi_sim.hpp            Simulator control API
src/wifi_manager.cpp        State, persistence, timeout, and worker logic
src/wifi_backend.hpp        Internal backend interface
src/backend_espwifi.cpp     Shared native/remote ESP-IDF Wi-Fi backend
src/backend_sim.cpp         Deterministic simulator backend
src/wifi_netif_native.cpp   Default native station netif
src/wifi_netif_remote.cpp   ESP-Hosted station netif workaround
test/test_wifi_manager.cpp  Host-side manager tests
```

Run the host-side tests from the repository root:

```sh
nix develop -c sh libs/wifi/test/run.sh
```
