# devkit_compression

Streaming decompressors. Each one pulls its input through a `Source` and hands
out as many bytes as the caller asks for, resuming where it stopped on the next
call, so neither side ever holds a whole stream.

| type | what |
|---|---|
| `Source` | the input every decompressor takes, as `std::io::BufRead` shapes it; `&[u8]` is one |
| `Inflate` | DEFLATE inside a zlib (`start`) or gzip (`start_gzip`) wrapper, or bare (`start_raw`) |
| `Lzw` | variable-width LZW up to 12-bit codes, in the GIF and the TIFF variants |

```rust
let mut z = heap::zeroed_box::<Inflate>(caps)?;
let mut window = heap::zeroed_box::<Window>(caps)?;
z.start(&mut src)?;
let n = z.read(&mut src, &mut window, &mut out)?;
```

**Nothing here allocates.** The 32 KB `Window` and the 16 KB `LzwWork` are the
caller's, because which heap they belong in is the caller's decision. `start`
with the same buffer begins the next stream, which is how a format made of many
independently compressed pieces reuses one.

**The source and the buffers are arguments of `read`, not fields.** A decoder
can hold the `Inflate`, its window and its source side by side without borrowing
from itself. The `Source` is `dyn` and called once per block, so the decode loop
does not pay for it.

`read` fills `out`; fewer bytes than asked means the stream ended. A corrupt
stream fails the call and every later one until `start`. A DEFLATE stream that
runs out of input fails with `Truncated` once it has read more than the few
zero bits a final symbol's lookahead needs; a bare one may also end quietly at a
block boundary. A wrapped stream ends after its trailer: the Adler-32 of a zlib
stream, the CRC-32 and length of a gzip member. One that does not match fails
with `Corrupt`; `ended` is true only after it matched. A gzip header's optional
fields are skipped and its header CRC checked.

**Input that arrives in pieces.** A `Source` whose `pending` is true has more
input to come after what `fill_buf` returns. `Inflate` then decodes while the
current block surely holds the next symbol, takes the rest one symbol at a time
without consuming a symbol it only has part of, and returns what it has when the
input runs out; `ended` tells that apart from the end of the stream.

Running out of input ends an LZW stream as an end-of-information code would:
writers often leave the code out, and a stream cut short is told apart by the
caller from the number of bytes it got.

## Tests

`test/run.sh` builds and runs the host tests inside the nix dev shell. It first
has `test/gen_fixtures.py` write the streams with Pillow (libtiff for TIFF) into
a temporary directory, inside `nix-shell`; the fixtures are not kept in git. The
streams decode to patterns the test regenerates.

`firmware/rust_compression_bench` times the crate on a board.
