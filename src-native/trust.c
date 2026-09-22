// Asks the engine's own TLS library whether one certificate parses, so that the
// Windows store can be handed over without the certificates it would choke on.
// LibreSSL reads a PEM bundle as all or nothing: one certificate it cannot
// decode empties the whole trust list.
#include <openssl/x509.h>

int ksip_x509_parses(const unsigned char *der, long length)
{
	const unsigned char *cursor = der;
	X509 *certificate = d2i_X509(NULL, &cursor, length);
	if (!certificate)
		return 0;
	X509_free(certificate);
	return 1;
}
