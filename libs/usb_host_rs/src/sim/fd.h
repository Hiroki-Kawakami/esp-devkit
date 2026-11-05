/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Socket calls whose flags and macros bindgen cannot bind.
 */

#pragma once

#include <stddef.h>
#include <sys/types.h>

/* One datagram carrying `name` and a descriptor. Returns the name's length,
   or -1 once the socket fails; *fd is -1 when none came with it. */
int usbh_sim_recv_fd(int sock, char *name, size_t capacity, int *fd);

/* send() that fails instead of raising SIGPIPE once the peer is gone. */
ssize_t usbh_sim_send(int fd, const void *data, size_t bytes);
