# Chainguard's OpenSSL 4.0 unregisters DSA from its default provider,
# and the extension is built with -DOPENSSL_NO_DSA, as Wolfi's Ruby is.
exclude(/\Atest_/, "DSA is not available in Chainguard's OpenSSL 4.0")
