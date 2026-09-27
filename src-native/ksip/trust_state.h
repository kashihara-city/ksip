// The trust store the engine verifies the SIP server's certificate with; see trust_state.cpp.
#pragma once
#include <re.h>

namespace trust_state {
// Adds "tls_trust_certificates": n while the SIP transport is TLS; nothing otherwise.
void write_state(odict *od);
} // namespace trust_state
