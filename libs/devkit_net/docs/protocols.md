# Protocols (`devkit_net::stack`)

What the stack does on the wire, protocol by protocol. Everything here runs in
the sans-IO `Stack`: `receive(iface, frame, now, tx)` takes a frame,
`poll(now, tx)` runs the timers and returns the next deadline, and both send
through `tx`.

## Scope

IPv4 only, over Ethernet-medium links (with ARP) and IP-medium links (no
link-layer header). No IP forwarding, multicast groups, IP options on send, SACK,
TCP timestamps or path MTU discovery.

## ARP

- A cache of 16 entries per stack; a full cache drops the resolved entry that
  expires first.
- A miss sends a broadcast request, again every second, and gives up after 3.
  While it waits, the entry holds the latest packet for that address only.
- An entry lives 300 s. One still in use is asked about again 30 s before it
  expires, by a request sent straight to the known MAC, so a busy peer never
  lapses into a broadcast.
- A request or reply updates an entry that exists; one aimed at the stack's own
  address also creates it.
- An address set on an interface is announced with 2 gratuitous ARPs, 2 s apart
  (RFC 5227).
- `arp_probe` asks who has an address without claiming it, for DHCP's conflict
  check.

## IPv4

- Received: the header checksum is checked, options are skipped, and the
  payload is cut to the total length so link padding is dropped.
- Dropped as sources (RFC 1122 3.2.1.3): broadcast, multicast, 240/4, 127/8
  from outside the loopback interface, and the subnet broadcast.
- Accepted destinations: the interface's address, the limited broadcast and the
  subnet broadcast. TCP and ICMP echo answer unicast only.
- Reassembly keeps up to 4 datagrams of at most 16 KiB for 30 s; a fifth replaces
  the one closest to its deadline, and one over 16 KiB is given up.
- Sent: TTL 64, no fragmentation. A packet larger than the interface's MTU fails
  with `MessageTooLarge`.
- Routing picks the first interface that is up whose subnet holds the
  destination, and otherwise the first interface with a gateway. The stack's own
  addresses and 127/8 go through the loopback interface.

## ICMP

- Echo requests to a unicast address are answered.
- A UDP datagram nobody takes and a unicast packet of an unknown protocol get
  port and protocol unreachable, quoting the IP header and 8 bytes. At most 10
  errors a second, and never about a broadcast, an unspecified sender or another
  ICMP error.
- Received unreachable, time exceeded and parameter problem messages go to the
  socket they quote:
  - TCP (RFC 5927): only when the quoted sequence number is in flight. Port or
    protocol unreachable ends a connection in SYN-SENT with
    `ConnectionRefused`; anything else is kept as a soft error, reported if the
    connection later times out.
  - UDP: only a connected socket hears of it (RFC 1122 4.1.3.3), as the error of
    its next receive.

## UDP

- Local ports come from 49152–65535, starting at a random point (RFC 6056).
- Checksums are always sent; a received zero checksum means none.
- A socket queues up to 64 datagrams within its receive buffer; one that does
  not fit is dropped.
- `send_to` returns `WouldBlock` when the link refuses the frame. A datagram
  waiting for ARP counts as sent; it is dropped if resolution fails or another
  datagram to the same host replaces it.

## TCP

- Options: MSS and window scale (up to 14) in SYN and SYN-ACK. Without the
  peer's MSS the stack assumes 536.
- Initial sequence numbers: a 4 µs clock plus SipHash of the connection with a
  random key (RFC 6528). Local ports as for UDP.
- Congestion control: RFC 3390 initial window, slow start and congestion
  avoidance, fast retransmit after 3 duplicate ACKs and NewReno recovery. The
  window starts over after an idle period longer than the RTO.
- RTO: 1 s initially, 200 ms to 60 s, from smoothed RTT samples; a retransmitted
  segment gives no sample. 12 retransmissions abort a connection with
  `ConnectionAborted`, 7 SYN retransmissions a connect with `TimedOut`, either
  replaced by a soft error from ICMP when there is one.
- Sending: sender silly window avoidance (RFC 1122 4.2.3.4) and Nagle, unless
  `set_nodelay`. A zero window is probed from one RTO on, for as long as the
  peer answers.
- Receiving: out-of-order segments are kept in the receive buffer and joined as
  the gap fills. ACKs are delayed up to 40 ms or until a second segment;
  out-of-order data and a FIN are acknowledged at once.
- RFC 5961: a RST that is not exactly at `rcv_nxt`, and any SYN on a
  synchronized connection, gets a challenge ACK instead of acting.
- Simultaneous open is handled. TIME-WAIT and FIN-WAIT-2 last 60 s.
- Keepalive, when set, probes a connection with nothing in flight after the
  idle time, then every 75 s, and gives up after 9 unanswered probes with
  `TimedOut`.
- A segment the link refuses is not counted as sent: output stops there and goes
  on when the link reports room (`LinkRx::tx_ready`) or 1 ms later. A refused
  ACK stays pending the same way.

## DHCP client

- DISCOVER and REQUEST with the broadcast flag, a client identifier from the
  MAC, the hostname from `Config`, a maximum message size of 1500, and a request
  for subnet, router, DNS (up to 3 kept) and lease time.
- Retransmission from 4 s doubling to 64 s, each ±1 s (RFC 2131 4.1).
- After a restart of the client it asks for the address it held before
  (INIT-REBOOT), twice, 2 s apart, before going back to DISCOVER.
- An offered address is probed twice, 500 ms apart (RFC 5227). If another host
  answers, the client sends DECLINE and starts over after 10 s. An ACK for
  another address than the one asked for is refused.
- The lease renews toward its server at T1 and rebinds toward anyone at T2,
  retrying no faster than every 60 s, and starts over when it runs out or on a
  NAK.
- Stopping the client (DHCP mode off, link down) sends RELEASE and clears the
  address.
