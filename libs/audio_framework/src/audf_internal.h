/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

#include "audf_eq.h"
#include "audf_fifo.h"
#include "audf_gain.h"
#include "audf_mixer.h"
#include "audf_resampler.h"

audf_fmt_t audf_eq_fmt(const audf_eq_t *eq);
uint8_t    audf_eq_channels(const audf_eq_t *eq);
audf_fmt_t audf_gain_fmt(const audf_gain_t *gain);
uint8_t    audf_gain_channels(const audf_gain_t *gain);
audf_fmt_t audf_mixer_fmt(const audf_mixer_t *mixer);
uint8_t    audf_mixer_out_channels(const audf_mixer_t *mixer);
uint8_t    audf_mixer_in_channels(const audf_mixer_t *mixer, uint8_t input);
audf_fmt_t audf_resampler_fmt(const audf_resampler_t *r);
uint8_t    audf_resampler_channels(const audf_resampler_t *r);
size_t     audf_resampler_max_in_frames(const audf_resampler_t *r);
audf_fmt_t audf_fifo_fmt(const audf_fifo_t *fifo);
uint8_t    audf_fifo_channels(const audf_fifo_t *fifo);
/* Changes on every flush. */
uint32_t   audf_fifo_generation(audf_fifo_t *fifo);
