# Finite-field DH (tmp_dh, the DH callback, anonymous DH ciphers) is not
# available in Chainguard's OpenSSL 4.0.
exclude(:test_connect_works_when_setting_dh_callback_to_nil, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
exclude(:test_get_ephemeral_key, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
exclude(:test_post_connect_check_with_anon_ciphers, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
exclude(:test_tlsext_hostname, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
exclude(:test_tmp_dh, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
exclude(:test_tmp_dh_callback, "fails against Chainguard's OpenSSL 4.0, which unregisters weak and legacy algorithms")
