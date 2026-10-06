// How many certificates are in the trust store baresip verifies the SIP
// server's TLS certificate with (sip_cafile, loaded by baresip's uag.c).
// baresip only warns when that file cannot be loaded, and every TLS
// registration then fails with nothing else to show for it. The store it
// actually uses says so directly, in every report, and not through words in
// the log.
#include <re.h>
#include <baresip.h>
#include <openssl/ssl.h>
#include <openssl/x509_vfy.h>
#include "trust_state.h"
#include <cstdint>

// libre's accessor for a TLS context's OpenSSL context: declared in its
// private src/tls/openssl/tls.h, defined in the static library.
extern "C" SSL_CTX *tls_ssl_ctx(const struct tls *tls);

namespace trust_state {
void write_state(odict *od) {
    // No TLS transport: there is no store to verify with, and nothing is said.
    const struct tls *tls = uag_tls();
    if (!tls) return;
    const SSL_CTX *context = tls_ssl_ctx(tls);
    X509_STORE *store = context ? SSL_CTX_get_cert_store(context) : nullptr;
    int64_t certificates = 0;
    if (store) {
        STACK_OF(X509_OBJECT) *objects = X509_STORE_get0_objects(store);
        for (int i = 0; objects && i < sk_X509_OBJECT_num(objects); ++i)
            if (X509_OBJECT_get_type(sk_X509_OBJECT_value(objects, i)) == X509_LU_X509) ++certificates;
    }
    odict_entry_add(od, "tls_trust_certificates", ODICT_INT, certificates);
}
} // namespace trust_state
