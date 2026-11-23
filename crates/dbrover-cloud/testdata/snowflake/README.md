# Test keys for Snowflake's key-pair sign-in

Made for `src/snowflake.rs`'s tests with OpenSSL 4.1 on 2026-11-23, and registered with no
Snowflake user: they sign nothing anywhere else.

- `key.p8`: a 2,048-bit RSA key in PKCS #8, as Snowflake's guide makes one
  (`openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048`).
- `key-pkcs1.pem`: the same key as PKCS #1 (`openssl rsa -traditional`).
- `key-aes.p8`, `key-scrypt.p8`, `key-des3.p8`: the same key encrypted with the passphrase
  `correct-horse` (`openssl pkcs8 -topk8` with `-v2 aes-256-cbc`, `-scrypt` and `-v2 des3`).
- `ec-key.p8`: a P-256 key, which Snowflake's SQL API doesn't take.

The public key's fingerprint, which the tests expect:
`openssl rsa -in key.p8 -pubout -outform DER | openssl dgst -sha256 -binary | openssl enc -base64`
prints `qlxIBmpm0dI1cnws2N11/8jsCrfLPEqNCdHES54RIXk=`.
