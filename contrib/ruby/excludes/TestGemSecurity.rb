# Chainguard's OpenSSL 4.0 unregisters DSA from its default provider.
exclude(:test_class_create_key_downcases, "DSA is not available in Chainguard's OpenSSL 4.0")
