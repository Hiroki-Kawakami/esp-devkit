# Performance

## Measuring

`firmware/esp_hosted_enhanced/development/netbench.py --ip <board> --host <host>`
drives the netbench ports from the PC: TCP sink (5001), TCP source (5002), UDP
echo (5003) and UDP control (5004). It reads the board's heap through
`devkit cmd`, turns Wi-Fi power save off, and reports TCP down and up, UDP echo
latency idle and under load, and with `--udp` UDP down and up.
`firmware/rust_hello` serves the ports from devkit_net and
`firmware/lwip_netbench` from lwIP, with the same buffer sizes (TCP 65534 on the
side that carries the stream, as lwIP's `TCP_WND`/`TCP_SND_BUF`).

Results move by several Mbps between runs with the radio environment; compare
builds over several runs, alternating them.

## ESP32-S3 (AtomS3 Lite)

240 MHz, 16 KB instruction cache, flash in QIO at 80 MHz, no PSRAM,
`DEVKIT_NET_IRAM_OPT=y`.

| | devkit_net | lwIP |
|---|---:|---:|
| TCP down | 29–33 Mbps | 7 Mbps |
| TCP up | 27–40 Mbps | 36 Mbps |
| UDP down | 37–43 Mbps | 39 Mbps |
| UDP up | 50–56 Mbps | 36 Mbps |
| UDP echo p50 under TCP down / up | 12 / 10 ms | 10 / 11 ms |

UDP up sends with `send_to_async`, so the sender waits for the link instead of
losing datagrams.

## ESP32-P4 with a C6 coprocessor (Tab5)

Wi-Fi through esp_hosted over SDIO, power save off.

| | devkit_net | lwIP |
|---|---:|---:|
| TCP down | 32–35 Mbps | 33–36 Mbps |
| TCP up | 22–23 Mbps | 22–23 Mbps |
| UDP up | 28–32 Mbps | 24–25 Mbps |

## Where the time goes on a small instruction cache

On the ESP32-S3 the stack is bound by instruction fetch, not by its algorithms.
With the code in flash, the core running the stack spends about two thirds of
its cycles stalled on instruction fetch during TCP down: about 11,000
instructions and 138,000 cycles per segment. Code that runs once per frame
(building an ACK, the link call) is evicted between frames by the Wi-Fi driver
and the other tasks, and runs at about 34 cycles per instruction; a hot loop such
as the checksum stays cached at about 3.5. The profile is flat: no function
stands out, and compiling for size or pinning the task to a core changes
little.

What does help is fetching less from flash per frame:

- `DEVKIT_NET_IRAM_OPT` places the per-frame path in IRAM: receive, IPv4 and
  TCP input, TCP output and ACKs, UDP input and send, ARP lookup, the checksum,
  queueing and waking around the stack's task, and the built-in Wi-Fi's receive, transmit and tx-done
  callbacks. That is about 19 KB of heap. Rare paths (ICMP, reassembly, ARP
  input, passive open, RST, SYN-SENT, the timers) are kept out of line so they
  stay in flash. With DIO flash, TCP down goes from 13 to about 30 Mbps and UDP
  down from 18 to about 39.
- QIO flash moves twice the bits per clock of DIO, which shortens each miss.
- A 32 KB instruction cache about halves the cycles per segment too, but costs
  16 KB of internal RAM.

## Memory

Socket buffers grow with the data (see the README), so idle and listening
sockets cost no buffer, and a connection reaches its full buffer only when the
peer fills it. On a board without PSRAM, a 64 KB TCP buffer, the IRAM option and
the Wi-Fi driver's 32 receive buffers together come close to the internal heap;
a smaller buffer leaves the driver room under load.

## Measuring cycles on Xtensa

The `perfmon` component reads the core's performance counters
(`xtensa_perfmon_init`, `_start`, `_value`), per core, from a task pinned to
the core being measured. Count with `kernelcnt` 0 and `tracelevel` 15; with
`kernelcnt` 1 the counters stay at zero. On the ESP32-S3 a cache miss shows up as
"instruction RAM busy" (`XTPERF_CNT_I_STALL`, mask `0x2`), not as an instruction
cache miss, because the cache sits outside the core.
