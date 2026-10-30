# devkit_audio

PCM processing modules — EQ, gain, mixer, resampler, FIFO, drift control and
codecs — that each work on their own, plus `graph` to wire them together.

## Sample formats

Modules exchange interleaved `i16` or `i32`, chosen by the module's type:
`Eq<i32>`, `fifo::new::<i16>()`. `i32` holds 24-bit PCM (full scale `1 << 23`) with 8
bits of headroom, so a boost in one module survives into the next; `i16`
saturates at every module boundary. Inside a module the arithmetic is always
wide. `convert` translates to and from the interchange layouts (16-bit, packed
24-bit, full-range 32-bit) at the edges. To change the format, create the module
again.

## Memory

Every module allocates at creation only, from the heap caps in its config, and
returns `Error::NoMemory` when a heap is full.

## Threading

A module and its control are created together:

```rust
let (mut eq, control) = Eq::<i32>::new(&EqConfig { channels: 2, enabled: true, ..Default::default() })?;
control.set_biquads(&[Biquad::peaking(48000, 1000.0, 1.0, 6.0)])?;
eq.process_in_place(&mut pcm);
```

`EqControl`, `GainControl`, `MixerControl` and `ResamplerControl` are `Clone +
Send`, and their setters are safe from any task. `process` picks the new values
up at its next call and never waits for a setter. `reconfig` and `process` take
`&mut self`, so they cannot run at the same time.

A FIFO has one `Writer` and one `Reader`, either of which may block.

## Graph

A graph is made of push and pull sections:

- `add_input` starts a push section: `Graph::write` runs it on the caller's task
  and returns once the sink has taken the data, so a sink that blocks paces the
  writer.
- `add_source` (a read closure) and a FIFO's output start pull sections:
  `Graph::read` pulls through them from a node with no consumer.
- A node with one input takes that input's direction. A node with several inputs
  may have at most one push input: with one it is part of the push section and
  pulls its other inputs by the same frame count, without one it is a pull node.
- A FIFO is the only node that turns push into pull. A sink and a fed FIFO need a
  push input; a pull node feeds at most one consumer.

`GraphBuilder::build` checks these rules and reports any mistake made while
adding nodes. The graph owns the modules moved into it; their controls stay with
the caller. Example: background music written from a decoder loop, mixed with a
microphone captured on its own clock.

```rust
let mut g = GraphBuilder::new(&GraphConfig::default());
let bgm = g.add_input::<i16>(1);
let mic = g.add_fifo(mic_reader);                      // fed by mic_writer.write()
let rs = g.add_resampler(mic, resampler, Some(drift));
let mix = g.add_mixer(&[bgm, rs], mixer);
g.add_sink::<i16>(mix, |pcm| write_to_device(pcm));
let mut graph = g.build()?;

loop {
    let n = decode(&mut pcm);
    graph.write(bgm, &pcm[..n])?;                      // blocks in write_to_device
}
```

A resampler given a drift steers its ratio from the fill level of the FIFO
upstream of it. The drift holds while that FIFO is filling to its prefill, and
starts over after `flush`.

## Codecs

Decoders and encoders take one encoded frame at a time; containers are parsed by
the caller. A concrete decoder has a `decode` typed for its samples, and the
`Decoder` and `Encoder` traits take `PcmMut`/`Pcm` for a caller that picks the
codec at run time. IMA ADPCM uses the WAV block layout (format tag 0x11). The AAC
decoder (LC, HE-AAC v1 and v2) is described in [`docs/aac.md`](docs/aac.md), the
FLAC decoder in [`docs/flac.md`](docs/flac.md), the ALAC decoder in
[`docs/alac.md`](docs/alac.md), the Opus decoder in [`docs/opus.md`](docs/opus.md)
and the MP3 and MP2 decoder in [`docs/mp3.md`](docs/mp3.md).

## Using it on ESP32-P4

AAC and MP3 use PIE kernels, in `src/aac/kernels_p4.S` and `src/mp3/kernels_p4.S`.
A task that decodes them needs a stack `Caps::SIMD` satisfies, and `pcm` a
`Caps::SIMD` buffer: PIE stores fault in RTC RAM.

The kernels' symbols carry a `devkit_` prefix, so the crate links next to
`audio_framework`.

## Speed

On an ESP32-P4 at 360 MHz, decoding a clip held in PSRAM with the decoder in
PSRAM; cost is a percentage of one core, best of 3 rounds, measured by
`firmware/rust_audio_bench` next to `audio_framework`.

| clip | devkit_audio | audio_framework |
|---|---:|---:|
| MP3 44.1 kHz stereo 128 kbps | 7.9 % | 8.5 % |
| MP3 44.1 kHz stereo 320 kbps | 9.6 % | 10.9 % |
| MP2 44.1 kHz stereo 256 kbps | 5.4 % | 5.5 % |
| AAC-LC 44.1 kHz stereo 128 kbps | 3.0 % | 3.0 % |
| HE-AAC v1 48 kHz stereo 64 kbps | 10.8 % | 10.9 % |
| HE-AAC v2 48 kHz 32 kbps | 10.0 % | 10.2 % |
| Opus CELT stereo 128 kbps | 13.5 % | 12.6 % |
| Opus SILK and Hybrid stereo | 10.4 % | 9.1 % |
| Opus SILK mono 8 kHz | 2.8 % | 2.7 % |
| FLAC 44.1 kHz 16-bit stereo | 4.7 % | 7.0 % |
| FLAC 96 kHz 24-bit stereo | 12.8 % | 19.3 % |
| ALAC 44.1 kHz 16-bit stereo | 7.8 % | 12.4 % |
| EQ 5 stages + gain, S32 stereo 48 kHz | 9.9 % | 9.3 % |

Code placement alone moves these by about 5 %, and so does where the heap puts
the buffers: over 16 heap offsets HE-AAC v1 averages 11.1 % and v2 10.3 %. HE-AAC
is limited by the L2 cache, which holds both flash code and PSRAM data. With the
256 KB L2 in place of 128 KB, v1 takes 8.6 % and v2 8.0 %. Instructions come from
flash at about 1500 cycles a line, so what is left on Opus is mostly fetch cost
of the larger code: with the hot functions in IRAM, SILK matches
`audio_framework`.

## Testing

`test/run.sh` builds and runs the host tests inside the nix dev shell. It
generates material with `libs/audio_framework/test/make_*_material.sh`, cuts it
into packets with `test/packetize.py`, decodes those with `audio_framework`
(`test/c_ref.c`) and compares every sample, the frame counts and the Opus final
range, also after a reset. Set `MATERIAL` to reuse a directory of generated
files.

`firmware/rust_audio_bench` times both on a board and checks they agree.
