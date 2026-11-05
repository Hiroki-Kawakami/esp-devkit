#!/usr/bin/env python3
# A TLS 1.3 server that answers each line with the connection's exported keying
# material for the label "devkit-test", 32 bytes, in hex. Arguments: port, key,
# certificate.
import socket
import sys

from OpenSSL import SSL

port, key, cert = int(sys.argv[1]), sys.argv[2], sys.argv[3]
ctx = SSL.Context(SSL.TLS_METHOD)
ctx.set_min_proto_version(SSL.TLS1_3_VERSION)
ctx.use_privatekey_file(key)
ctx.use_certificate_file(cert)
listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", port))
listener.listen(4)
while True:
    sock, _ = listener.accept()
    conn = SSL.Connection(ctx, sock)
    conn.set_accept_state()
    try:
        conn.do_handshake()
        line = b""
        while not line.endswith(b"\n"):
            line += conn.recv(4096)
        conn.sendall(conn.export_keying_material(b"devkit-test", 32).hex().encode() + b"\n")
        conn.shutdown()
    except (SSL.Error, OSError):
        pass
    sock.close()
