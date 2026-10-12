exclude(:test_digest_constants, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
exclude(:test_sha2, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
exclude(:test_sha3, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
exclude(:test_sha512_truncate, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
