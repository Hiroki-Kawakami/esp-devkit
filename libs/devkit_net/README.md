# devkit_net

An IPv4 network stack in Rust that replaces lwIP: ARP, IPv4 with reassembly,
ICMP, UDP, TCP and a DHCP client, behind a std-like socket API. It runs the same
over the chip's own Wi-Fi, a Wi-Fi coprocessor registered by a board, and the
simulator's network. How each protocol behaves is in
[`docs/protocols.md`](docs/protocols.md); how the stack spends its time and how
to measure it is in [`docs/performance.md`](docs/performance.md); what it relies
on in ESP-IDF, private APIs included, is in [`docs/idf.md`](docs/idf.md).

```rust
devkit_net::start(devkit_net::Config::default())?;
let wifi = devkit_net::wifi().ok_or(Error::Unsupported)?;
wifi.connect(&StaConfig::new("YOUR_SSID", "YOUR_PASSWORD")).await?;
let address = wifi.sta().wait_ipv4_async().await.address;

let stream = TcpStream::connect("192.168.x.x:5001".parse()?)?;
stream.write_all(b"hello")?;
```

| module | what |
|---|---|
| `iface` | `start`, `add_link`, `interfaces` and `Interface`: link state, statistics, IPv4 mode and address, DNS servers |
| `wifi` | `Wifi` (connect, disconnect, power save, state), the `WifiDriver` trait and `register_wifi` |
| `udp_socket`, `tcp_socket` | `UdpSocket`, `TcpListener`, `TcpStream` and its split halves, with their builders |
| `link` | the `Link` trait a network interface implements, `LinkRx` and `RxFrame` |
| `stack` | the sans-IO `Stack` that does all protocol work |
| `net` | the stack's task, the lock around the `Stack`, and the wakeups between them |
| `wifi_native` | the chip's own Wi-Fi station (`CONFIG_DEVKIT_NET_WIFI`) |
| `sim` | the simulator's radio, talking to libslirp in the devkit host |

## Starting

`start(config)` spawns the stack's task once. Links may be added before or after
it; frames a link receives before the start are dropped. The first call into the
crate registers the radio the firmware has without a board's help: the chip's own
Wi-Fi when `CONFIG_DEVKIT_NET_WIFI` is set, the simulator's radio on the host. A
board with a Wi-Fi coprocessor registers its driver with `register_wifi`; there
is one radio per firmware, and `wifi()` returns it.

`Config` holds the default socket buffer sizes and heap caps, the hostname sent
to DHCP servers, and the priority, core and stack of the stack's task.

## Interfaces

Every link becomes an `Interface`. `add_link` starts it with `Ipv4Mode::Off`;
the radio's station interface is named `sta` and starts with `Ipv4Mode::Dhcp`.
`set_ipv4` switches between off, a static address and DHCP. A DHCP client runs
on the stack's task while its interface is in `Dhcp` mode and the link is up,
and releases the lease when either stops. `wait_ipv4` (and `_timeout`,
`_async`) returns once an address is configured.

An interface named `lo` holds 127.0.0.1/8. A packet to one of the stack's own
addresses also goes through it.

## Wi-Fi

`Wifi::connect` leaves the current network first and returns once the station
has joined or failed to; the address comes later, from DHCP. `state()` and `changed()` follow the connection.
`set_power_save` takes `None`, `Min` (the default) or `Max`, and is kept for a
radio that is not up yet. A `WifiDriver` runs its blocking calls wherever it
likes and returns boxed futures, so the caller's executor goes on meanwhile.

## Sockets

Every call that waits comes in three forms: blocking, `_timeout`, and `_async`.
The blocking ones park the calling task, so on a task that runs an executor they
stall its other futures meanwhile; the async ones run on a devkit executor.

Socket buffers are allocated as data arrives, doubling up to the size the
socket was given (`Config` or the builder's `rx_buffer`/`tx_buffer`), and stay
until the socket closes. The TCP window is advertised from the full size; a
segment that finds no memory is dropped and comes again. A socket that is only
listening or idle holds no buffer.

`UdpSocket::send_to` hands the datagram to the link before returning, and fails
with `WouldBlock` when the link has no room for it. `send_to_async` waits for
room instead. TCP never reports a full link: output stops at the segment the
link refused and goes on once it has room.

A `TcpStream` splits into a `TcpReader` and a `TcpWriter` that may live on
different tasks. Keepalive is off until `set_keepalive`; Nagle is on until
`set_nodelay(true)`.

## Threading

The whole `Stack` sits behind one mutex. The stack's task takes it for each
received frame and timer; a socket call takes it for its own work, sends what
that produced from the caller's task, and wakes the stack's task only when the
call brought a timer forward. Links deliver frames from their own tasks into a
queue of 64 that the stack's task drains.

## Writing a link

A `Link` reports its `LinkInfo` (Ethernet or IP medium, MAC, MTU), is given a
`LinkRx` once in `attach`, and sends with `transmit`. Through the `LinkRx` it
delivers received frames and reports the link going up and down, from its own
tasks, never from an interrupt.

- `RxFrame` borrows the link's buffer and calls the link's release function
  when the stack is done with it, so a link can lend its receive buffers without
  a copy.
- `transmit` borrows the frame for the call only. `Busy` means the link has no
  room; a link that can tell when it has room again calls `LinkRx::tx_ready`,
  which is cheap enough to call for every frame sent. Senders waiting on a link
  that never calls it retry every millisecond.

## Configuration

| Kconfig | default | what |
|---|---|---|
| `DEVKIT_NET_WIFI` | y where the chip has Wi-Fi | registers the chip's own Wi-Fi as `sta` |
| `DEVKIT_NET_IRAM_OPT` | n | places the code every frame goes through in IRAM (about 19 KB) |

`DEVKIT_NET_IRAM_OPT` trades heap for speed where flash cache misses dominate;
see [`docs/performance.md`](docs/performance.md). A new Kconfig symbol reaches an
existing `sdkconfig` after `idf.py reconfigure`.

## Simulator

The simulator's radio joins any network at once and exchanges Ethernet frames
with the devkit host over `SIM_NET_FD`; the host runs libslirp behind it, so the
firmware reaches the PC's network through NAT. See `docs/simulator.md` at the
repository root.

## Testing

The host tests run several `Stack`s against each other over simulated wires
(`src/harness.rs`) that can delay frames, lose them, or have the sending link
refuse them, and check the protocols frame by frame:

```sh
cmake --build firmware/rust_hello/simulator/build --target rust_test_devkit_net
```

`firmware/rust_hello` is the board check firmware: it joins the network saved
with `wifi join` and serves the netbench ports. `firmware/lwip_netbench` serves
the same ports from lwIP for comparison.
