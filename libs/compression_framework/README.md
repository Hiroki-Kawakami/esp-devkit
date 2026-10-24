# compression_framework

Streaming decompressors. Each one pulls its input through a `cmpf_src_t`
(`refill()` hands over the next block) and hands out as many bytes as the
caller asks for, resuming where it stopped on the next call, so neither side
ever holds a whole stream.

| header | what |
|---|---|
| `cmpf_source.h` | the input callback every decompressor takes |
| `cmpf_inflate.h` | DEFLATE inside a zlib wrapper |
| `cmpf_lzw.h` | variable-width LZW up to 12-bit codes, in the GIF and the TIFF variants |

**Nothing here allocates.** The inflate window (32 KB) and the LZW tables
(16 KB) are the caller's, because which heap they belong in is the caller's
decision: on a chip with little internal RAM, a decompressor that picked its own
heap would take memory the rest of the program is counting on. Calling `init`
again with the same buffer starts the next stream, which is how a format made of
many independently compressed pieces reuses one.

Running out of input ends an LZW stream as an end-of-information code would:
writers often leave the code out, and a stream cut short is told apart by the
caller from the number of bytes it got.

## Tests

`test/run.sh` builds and runs the host tests with gcc. It first has
`test/gen_fixtures.py` write `cmpf_fixtures.h` with Pillow (libtiff for TIFF),
inside `nix-shell`; the fixtures are not kept in git. The streams decode to
patterns the test regenerates in C.
