/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "audf_aac_sbr.h"

esp_err_t aac_ps_create(aac_ps_t **out, uint32_t caps);
void aac_ps_destroy(aac_ps_t *ps);
void aac_ps_reset(aac_ps_t *ps);
int  aac_ps_parse(aac_ps_t *ps, aac_bits_t *b, int bits);
bool aac_ps_ready(const aac_ps_t *ps);
/* A frame is aac_ps_begin, aac_ps_slots over slots 0-31 in order, aac_ps_end.
 * x_low is the first of the frame's 38 QMF rows. l is replaced in place. */
void aac_ps_begin(aac_ps_t *ps, const int32_t *x_low, int top);
void aac_ps_slots(aac_ps_t *ps, int32_t *l, int32_t *r, int n0, int count);
void aac_ps_end(aac_ps_t *ps);
