# devkit_net

An IPv4 network stack in Rust that replaces lwIP: ARP, IPv4 with reassembly,
ICMP, UDP, TCP, a DHCP client and a DNS resolver, behind a std-like socket API,
with TLS 1.3 from `devkit_crypto` and an HTTP/1.1 client on top. It runs the same
over the chip's own Wi-Fi, a Wi-Fi coprocessor registered by a board, and the
simulator's network. How each protocol behaves is in
[`docs/protocols.md`](docs/protocols.md); how the stack spends its time and how
to measure it is in [`docs/performance.md`](docs/performance.md); what it relies
on in ESP-IDF, private APIs included, is in [`docs/idf.md`](docs/idf.md).

```rust
devkit_net::start(devkit_net::Config::default())?;
let wifi = devkit_net::wifi().ok_or(Error::Unsupported)?;
wifi.start().await?;
wifi.connect(&StaConfig::new("YOUR_SSID", "YOUR_PASSWORD")).await?;
let address = wifi.sta().wait_ipv4_async().await.address;

let stream = TcpStream::connect("example.com:80")?;
stream.write_all(b"hello")?;
```

| module | what |
|---|---|
| `iface` | `start`, `add_link`, `interfaces` and `Interface`: link state, statistics, IPv4 mode and address, DNS servers |
| `dns` | `lookup`: host names to addresses |
| `addr` | `ToSocketAddrs`, what `connect` takes |
| `wifi` | `Wifi` (start, stop, connect, disconnect, scan, RSSI, power save, state), the `WifiDriver` trait and `register_wifi` |
| `wifi_manager` | saved networks, joining them by itself, the Wi-Fi on/off switch |
| `udp_socket`, `tcp_socket` | `UdpSocket`, `TcpListener`, `TcpStream` and its split halves, with their builders |
| `tls` | `TlsStream`: a `devkit_crypto` TLS 1.3 client connection over a `TcpStream`, and its split halves |
| `http` | `Client`, `Response`, `BodyWriter`, and `get` for a single request: HTTP/1.1 over TCP or TLS |
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
and releases the lease when either stops. DHCP also gives the DNS servers; in
`Static` mode `set_dns_servers` sets them, after `set_ipv4`, which clears them. `wait_ipv4` (and `_timeout`,
`_async`) returns once an address is configured.

An interface named `lo` holds 127.0.0.1/8. A packet to one of the stack's own
addresses also goes through it.

## Wi-Fi

The radio stays stopped until `Wifi::start`; `stop` leaves the network and
stops it. `connect` and `scan` on a stopped radio fail with
`ConnectError::Stopped`. `Wifi::connect` leaves the current network first and
returns once the station has joined or failed to; the address comes later, from
DHCP. `state()` and `changed()` follow the radio and the connection.
`set_power_save` takes `None`, `Min` (the default) or `Max`, and is kept for a
radio that is not started yet. A `WifiDriver` runs its blocking calls wherever it
likes and returns boxed futures, so the caller's executor goes on meanwhile.

## Wi-Fi manager

`wifi_manager::start()` runs nothing before it is called, and is called once
after `start`. It reads the saved networks and the on/off switch from NVS
(namespace `wifi_manager`), starts the radio when Wi-Fi is on (the default), and
from then on joins saved networks by itself on the stack's task. A firmware that
drives `Wifi` itself does not call it; using both is not supported.

| call | what |
|---|---|
| `set_enabled` / `enabled` | Wi-Fi on or off, kept in NVS; off stops the radio |
| `join(ssid, password, hidden)` | joins and saves once joined; `JoinError::Disabled` while off |
| `join_saved(ssid)` | joins with what was saved |
| `add` / `forget` / `networks` | edits the saved list; passwords never come back out |
| `disconnect` / `reconnect` | stops and resumes joining by itself |
| `scan` / `watch_scan(interval)` | networks in range by SSID, for a settings screen; a `ScanWatch` scans until dropped |
| `state` / `changed` | `Disabled`, `Idle`, `Scanning`, `Joining`, `Connected` (with RSSI, polled every 5 s), `Waiting` |

Up to 5 networks are kept, one NVS key each; a sixth replaces the one joined
longest ago. Each is sealed with ChaCha20 and a SipHash tag under a key derived
from the chip's factory MAC, so a flash dump does not show passwords in plain
text. Anyone with the chip and this source can still read them. A network that
does not open on the chip it is read on is treated as not saved.

Joining by itself scans first, then tries the saved networks it saw, strongest
first with 10 dB extra for the one joined last and those under -80 dBm at the
end; hidden networks the scan did not see come after. When none joins, it scans
again after 10 s, doubling up to 5 min. A network that refused its password
twice is skipped until `join`, `join_saved`, `add` or `reconnect`. A dropped
connection is retried on the same network once before scanning. `join` takes
over from it at once; when `join` fails, joining by itself resumes and finds
the network it left.

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

`TcpStream::connect` and `UdpSocket::connect` take what `ToSocketAddrs`
covers, as in std: a `SocketAddr`, `(ip, port)`, `("host", port)`,
`"host:port"` or a slice of addresses. A host is looked up first, inside the
`_timeout` given. TCP tries each address in turn; UDP takes the first IPv4 one.

A `TcpStream` splits into a `TcpReader` and a `TcpWriter` that may live on
different tasks. Keepalive is off until `set_keepalive`; Nagle is on until
`set_nodelay(true)`.

## TLS

`TlsStream` drives a `devkit_crypto::tls::Connection` over a `TcpStream`, with
the same three forms of every call. `connect(host, port, config)` looks the host
up, connects and completes the handshake, with `host` as the name the
certificate has to carry; `start(tcp, server_name, config)` begins on a stream
already in use, as STARTTLS does. `read` returns `Ok(0)` after the server's
close_notify; a stream that ends without one fails with `ConnectionAborted`,
since its data may be cut short. `shutdown` sends close_notify, then FIN. TLS
failures come back as `Error::Tls`. `connection()` gives the ALPN protocol, the
server's certificates and exported keying material. After the handshake a
`TlsStream` splits into a `TlsReader` and a `TlsWriter` that may live on
different tasks, as a `TcpStream` does.

```rust
let config = Arc::new(ClientConfig::new(Verify::WebPki(TrustStore::default_bundle()?)));
let mut tls = TlsStream::connect("example.com", 443, config)?;
tls.write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")?;
```

A handshake verifies the server's chain on the calling task: with
`Verify::WebPki` it took about 5 KB of stack on the devices.

## HTTP

`http::Client` sends HTTP/1.1 requests to `http://` and `https://` URLs and
keeps each connection whose response was read to its end, up to `max_idle`
(4), for the next request to the same scheme, host and port. A kept
connection the server closed meanwhile gets the request again on a new one.
`http::get(url)` makes a client for one request; it goes with the response.

```rust
let client = http::Client::new();
let mut resp = client.get("https://example.com/data.json").send()?;
let body = resp.bytes(64 * 1024)?;

let http::Started::Body(mut up) = client.put(url).content_length(n).start()? else { ... };
up.write_all(&chunk)?;
let resp = up.finish()?;
```

- **Bodies.** `Response::read` streams the body, `bytes(limit)` collects it and
  `save(path)` writes it to a file, leaving what it wrote when it fails. A
  response dropped before its end closes its connection.
  `content_length()` is the field as sent and `received()` counts the same
  bytes, so the two give progress even while the body is decoded.
- **Compression.** With `DEVKIT_NET_HTTP_DECOMPRESS` the client sends
  `Accept-Encoding: gzip, deflate` and decodes such bodies; `deflate` is
  taken with or without its zlib header. A request that sets
  Accept-Encoding itself gets the body as sent. `bytes(limit)` limits the
  decoded length; a Content-Length over it, or over what a compressor can
  make of that much, fails at once with `MessageTooLarge`. Decoding takes
  about 41 KB of heap per response, allocated at its first read.
- **Redirects.** 301, 302, 303, 307 and 308 are followed, 20 at most
  (`redirects`), as curl and wget do: 303, and 301 or 302 after a POST,
  become a GET; credentials and Authorization and Cookie fields are dropped
  when the scheme, host or port changes. Past the limit, `TooManyRedirects`.
- **Authentication.** Credentials from `credentials(user, password)` or the
  URL go out as Basic with the first request; a 401 that offers Digest
  (MD5 or SHA-256, `-sess` too, qop `auth`) gets one answer.
- **Request bodies.** `body(bytes)` is sent again when a redirect or a Digest
  challenge asks. `start()` streams one on a connection of its own, chunked
  unless `content_length` is given. A body over 1 MB, a streamed one of unknown
  length and one sent with credentials ask with `Expect: 100-continue` first;
  the body goes after 100 Continue or a second of silence. A server that
  answers before it gets the body hands `start` its response instead
  (`Started::Response`), after the redirects and the Digest challenge that do
  not need the body.
- **Timeouts.** `connect_timeout` (name lookup, TCP and TLS) and
  `idle_timeout` (each wait for the server) are 30 s; `timeout`, from the
  request to the end of the body, is off. All of them fail with `TimedOut`.
- **TLS.** Without `tls(config)`, the default CA bundle verifies servers; it
  is loaded at the client's first https request.

## Names

`dns::lookup` (and `_timeout`, `_async`) returns the addresses of a host name.
An address literal comes back as it is and `localhost` is 127.0.0.1; other
names go to the DNS servers of the interfaces that are up, in interface order.
Lookups of one name share a query, which runs on the stack's task and goes on
when its caller gives up, so its answer still lands in the cache. A blocking
lookup on the stack's task never returns. Errors: `NotFound` for a name that
does not exist or has no address, `NoNameServer`, `TimedOut`, and
`InvalidInput` for a name a query cannot carry (ASCII only). How the resolver
behaves on the wire is in [`docs/protocols.md`](docs/protocols.md).

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
| `DEVKIT_NET_HTTP_DECOMPRESS` | y | decodes gzip and deflate HTTP bodies; off, `devkit_compression` stays out of the link |

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
with `wifi join`, serves the netbench ports, and looks names up with
`dns <host>`. `firmware/lwip_netbench` serves
the same ports from lwIP for comparison.
