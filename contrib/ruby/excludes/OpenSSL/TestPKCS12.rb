# Every PKCS#12 test fails the same way on x86_64 and aarch64 against
# Chainguard's OpenSSL 4.0.
exclude(/\Atest_(create|dup|new_with)/, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
