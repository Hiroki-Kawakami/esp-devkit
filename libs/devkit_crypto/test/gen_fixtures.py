#!/usr/bin/env python3
# Writes the test's fixtures into a directory: hashes, MACs and KDF output from
# hashlib and hmac over inputs the test regenerates, and AEAD, key agreement and
# signature vectors from the cryptography package.
import hashlib
import hmac
import os
import sys

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, ed25519, padding, rsa, utils, x25519
from cryptography.hazmat.primitives.ciphers.aead import AESGCM, ChaCha20Poly1305

LENGTHS = [0, 1, 3, 55, 56, 63, 64, 65, 111, 112, 127, 128, 129, 200, 1000, 4099]
KEY_LENGTHS = [0, 1, 20, 32, 64, 65, 128, 131]
HASHES = ["sha1", "sha256", "sha384", "sha512", "blake2s"]


def noise(n, seed):
    out = bytearray()
    x = seed
    for _ in range(n):
        x = (x * 1103515245 + 12345) & 0x7FFFFFFF
        out.append((x >> 16) & 0xFF)
    return bytes(out)


def hkdf(name, salt, ikm, info, length):
    prk = hmac.new(salt, ikm, name).digest()
    out = b""
    block = b""
    i = 1
    while len(out) < length:
        block = hmac.new(prk, block + info + bytes([i]), name).digest()
        out += block
        i += 1
    return prk, out[:length]


def main(out_dir):
    lines = []
    for name in HASHES:
        for n in LENGTHS:
            lines.append(f"hash {name} {n} {hashlib.new(name, noise(n, n)).hexdigest()}")
        for k in KEY_LENGTHS:
            for n in [0, 64, 1000]:
                tag = hmac.new(noise(k, 7 + k), noise(n, n), name).hexdigest()
                lines.append(f"hmac {name} {k} {n} {tag}")
        for salt, ikm, info, length in [(0, 22, 10, 42), (80, 80, 80, 82), (0, 22, 0, 255)]:
            prk, okm = hkdf(name, noise(salt, 1), noise(ikm, 2), noise(info, 3), length)
            lines.append(f"hkdf {name} {salt} {ikm} {info} {length} {prk.hex()} {okm.hex()}")
    for k in [1, 16, 32]:
        for size in [16, 32]:
            for n in [0, 64, 65, 1000]:
                digest = hashlib.blake2s(noise(n, n), key=noise(k, 7 + k), digest_size=size).hexdigest()
                lines.append(f"blake2s_keyed {k} {size} {n} {digest}")
    lines += asymmetric()
    with open(os.path.join(out_dir, "vectors.txt"), "w") as f:
        f.write("\n".join(lines) + "\n")


def raw_point(key):
    return key.public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)


def asymmetric():
    lines = []
    for name, cls, key_len in [("aes128gcm", AESGCM, 16), ("aes256gcm", AESGCM, 32), ("chacha20poly1305", ChaCha20Poly1305, 32)]:
        for n, aad in [(0, 0), (1, 13), (16, 0), (17, 5), (1000, 64)]:
            key, nonce = noise(key_len, n + 1), noise(12, n + 2)
            sealed = cls(key).encrypt(nonce, noise(n, n + 3), noise(aad, n + 4))
            lines.append(f"aead {name} {n} {aad} {sealed.hex()}")
    for _ in range(3):
        a, b = x25519.X25519PrivateKey.generate(), x25519.X25519PrivateKey.generate()
        a_raw = a.private_bytes(serialization.Encoding.Raw, serialization.PrivateFormat.Raw, serialization.NoEncryption())
        b_pub = b.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
        lines.append(f"x25519 {a_raw.hex()} {b_pub.hex()} {a.exchange(b.public_key()).hex()}")
    curves = [("p256", ec.SECP256R1(), hashes.SHA256()), ("p384", ec.SECP384R1(), hashes.SHA384()), ("p521", ec.SECP521R1(), hashes.SHA512())]
    for name, curve, h in curves:
        a, b = ec.generate_private_key(curve), ec.generate_private_key(curve)
        if name != "p521":
            d = a.private_numbers().private_value.to_bytes(curve.key_size // 8, "big")
            lines.append(f"ecdh {name} {d.hex()} {raw_point(b.public_key()).hex()} {a.exchange(ec.ECDH(), b.public_key()).hex()}")
        for n in [0, 100]:
            msg = noise(n, n + 5)
            sig = a.sign(msg, ec.ECDSA(h))
            lines.append(f"ecdsa {name} {raw_point(a.public_key()).hex()} {n} {sig.hex()}")
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    private = key.private_bytes(serialization.Encoding.DER, serialization.PrivateFormat.TraditionalOpenSSL, serialization.NoEncryption())
    public = key.public_key().public_bytes(serialization.Encoding.DER, serialization.PublicFormat.PKCS1)
    lines.append(f"rsa_key {private.hex()} {public.hex()}")
    for hname, h in [("sha256", hashes.SHA256()), ("sha384", hashes.SHA384()), ("sha512", hashes.SHA512())]:
        msg = noise(77, 6)
        pkcs1 = key.sign(msg, padding.PKCS1v15(), h)
        pss = key.sign(msg, padding.PSS(mgf=padding.MGF1(h), salt_length=h.digest_size), h)
        lines.append(f"rsa_sig {hname} {pkcs1.hex()} {pss.hex()}")
    for hname, h in [("sha1", hashes.SHA1()), ("sha256", hashes.SHA256())]:
        digest = noise(h.digest_size, 9)
        sig = key.sign(digest, padding.PKCS1v15(), utils.Prehashed(h))
        lines.append(f"rsa_prehashed {hname} {sig.hex()}")
    for n in [0, 1, 300]:
        seed = noise(32, n + 7)
        k = ed25519.Ed25519PrivateKey.from_private_bytes(seed)
        pub = k.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
        lines.append(f"ed25519 {seed.hex()} {pub.hex()} {n} {k.sign(noise(n, n + 8)).hex()}")
    return lines


if __name__ == "__main__":
    main(sys.argv[1])
