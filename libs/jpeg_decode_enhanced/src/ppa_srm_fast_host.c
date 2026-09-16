/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * jpeg_decode_enhanced — PPA SRM submission path (host). Runs each strip
 * synchronously through the idf_compat PPA shim.
 */

#include <stdlib.h>
#include "ppa_srm_fast.h"

struct ppa_srm_fast_s {
    ppa_client_handle_t client;
    ppa_srm_fast_done_cb_t cb;
    void *cb_ctx;
    ppa_srm_oper_config_t op;
};

esp_err_t ppa_srm_fast_new(ppa_srm_fast_done_cb_t cb, void *ctx, ppa_srm_fast_handle_t *out)
{
    if (!cb || !out) return ESP_ERR_INVALID_ARG;
    ppa_srm_fast_handle_t h = calloc(1, sizeof(*h));
    if (!h) return ESP_ERR_NO_MEM;
    h->cb = cb;
    h->cb_ctx = ctx;
    ppa_client_config_t pcfg = { .oper_type = PPA_OPERATION_SRM };
    esp_err_t err = ppa_register_client(&pcfg, &h->client);
    if (err != ESP_OK) {
        free(h);
        return err;
    }
    *out = h;
    return ESP_OK;
}

void ppa_srm_fast_del(ppa_srm_fast_handle_t h)
{
    if (!h) return;
    ppa_unregister_client(h->client);
    free(h);
}

esp_err_t ppa_srm_fast_begin_frame(ppa_srm_fast_handle_t h, const ppa_srm_oper_config_t *tmpl)
{
    if (!h || !tmpl) return ESP_ERR_INVALID_ARG;
    h->op = *tmpl;
    h->op.mode = PPA_TRANS_MODE_BLOCKING;
    return ESP_OK;
}

esp_err_t ppa_srm_fast_submit(ppa_srm_fast_handle_t h, void *in_buf, uint32_t in_pic_h,
                              uint32_t in_offset_y, uint32_t in_block_h,
                              uint32_t out_offset_x, uint32_t out_offset_y)
{
    ppa_srm_oper_config_t op = h->op;
    op.in.buffer = in_buf;
    op.in.pic_h = in_pic_h;
    op.in.block_offset_y = in_offset_y;
    op.in.block_h = in_block_h;
    op.out.block_offset_x = out_offset_x;
    op.out.block_offset_y = out_offset_y;
    esp_err_t err = ppa_do_scale_rotate_mirror(h->client, &op);
    if (err == ESP_OK) h->cb(h->cb_ctx);
    return err;
}

esp_err_t ppa_srm_fast_abort(ppa_srm_fast_handle_t h)
{
    return h ? ESP_OK : ESP_ERR_INVALID_ARG;
}

void ppa_srm_fast_end_frame(ppa_srm_fast_handle_t h)
{
    (void)h;
}
