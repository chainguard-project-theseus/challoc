# OpenSSL 4.0 no longer reaches the second message this test expects for
# X509_V_ERR_INVALID_CA on a bare store context.
exclude(:test_verify_certificate_extra_message, "OpenSSL 4.0 reports INVALID_CA without the extra certificate line")
