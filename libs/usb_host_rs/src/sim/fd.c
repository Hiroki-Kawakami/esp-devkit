/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "fd.h"

#include <string.h>
#include <sys/socket.h>
#include <sys/uio.h>

int usbh_sim_recv_fd(int sock, char *name, size_t capacity, int *fd) {
    union {
        struct cmsghdr header;
        char bytes[CMSG_SPACE(sizeof(int))];
    } control;
    struct iovec iov = { .iov_base = name, .iov_len = capacity };
    struct msghdr msg = {
        .msg_iov = &iov,
        .msg_iovlen = 1,
        .msg_control = control.bytes,
        .msg_controllen = sizeof(control.bytes),
    };
    *fd = -1;
    ssize_t n = recvmsg(sock, &msg, 0);
    if (n <= 0) {
        return -1;
    }
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&msg); c; c = CMSG_NXTHDR(&msg, c)) {
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
            memcpy(fd, CMSG_DATA(c), sizeof(int));
        }
    }
#ifdef SO_NOSIGPIPE
    if (*fd >= 0) {
        int one = 1;
        setsockopt(*fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));
    }
#endif
    return (int)n;
}

ssize_t usbh_sim_send(int fd, const void *data, size_t bytes) {
#ifdef MSG_NOSIGNAL
    return send(fd, data, bytes, MSG_NOSIGNAL);
#else
    return send(fd, data, bytes, 0);
#endif
}
