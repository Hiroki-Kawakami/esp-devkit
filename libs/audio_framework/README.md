# audio_framework

PCM processing modules — EQ, gain, mixer, resampler, FIFO, drift control and
codecs — that each work on their own, plus `audf_graph` to wire them together.

## Sample formats

Modules exchange interleaved `AUDF_FMT_S16` or `AUDF_FMT_S32`, chosen per
instance at creation. S32 holds 24-bit PCM (full scale `1 << 23`) with 8 bits
of headroom, so a boost in one module survives into the next; S16 saturates at
every module boundary but runs in place on the caller's buffer. Inside a
module the arithmetic is always wide. `audf_convert*` translates to and from
the interchange layouts (16-bit, packed 24-bit, full-range 32-bit) at the edges.

## Memory

Every module allocates at create time only, from the heap caps in its config's
`alloc_caps`; 0 keeps plain `malloc`, which on ESP-IDF puts small blocks in
internal RAM.

## Threading

Setters (`audf_eq_set_biquads`, `audf_gain_set`, `audf_mixer_set_matrix`,
`audf_resampler_set_adjust`) are safe from any task. `process` picks the new
values up at its next call and never waits for a setter. `*_reconfig` must not
run concurrently with `process`.

## Graph

A graph is made of push and pull sections:

- `audf_graph_add_input` starts a push section: `audf_graph_write` runs it on
  the caller's task and returns once the sink has taken the data, so a sink
  that blocks paces the writer.
- `audf_graph_add_source` (a read callback) and a FIFO's output start pull
  sections: `audf_graph_read` pulls through them from a node with no consumer.
- A node with one input takes that input's direction. A node with several
  inputs may have at most one push input: with one it is part of the push
  section and pulls its other inputs by the same frame count, without one it is
  a pull node.
- A FIFO is the only node that turns push into pull. A sink and a fed FIFO need
  a push input; a pull node feeds at most one consumer.

`audf_graph_build` checks these rules. Example: background music written from
a decoder loop, mixed with a microphone captured on its own clock.

```c
audf_node_t *bgm = audf_graph_add_input(g, AUDF_FMT_S16, 1);
audf_node_t *mic = audf_graph_add_fifo(g, NULL, mic_fifo);      /* fed by audf_fifo_write */
audf_node_t *rs  = audf_graph_add_resampler(g, mic, resampler, &drift);
audf_node_t *mix = audf_graph_add_mixer(g, (audf_node_t *const[]){ bgm, rs }, mixer);
audf_graph_add_sink(g, mix, write_to_device, NULL);
audf_graph_build(g);

for (;;) {
    decode(pcm, &n);
    audf_graph_write(g, bgm, pcm, n);   /* blocks in write_to_device */
}
```

A resampler given a drift steers its ratio from the fill level of the FIFO
upstream of it. The drift holds while that FIFO is filling to its prefill, and
starts over after `audf_fifo_flush`.

## Codecs

Decoders and encoders take one encoded frame at a time; containers are parsed
by the caller. IMA ADPCM uses the WAV block layout (format tag 0x11).

## Testing

`test/run.sh` builds and runs the host test (inside the nix dev shell).
