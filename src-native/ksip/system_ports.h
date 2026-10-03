// Calls whose RTP and RTCP are on ports the system picked ("rtp_ports 0",
// the libre change in scripts/build/patch-baresip.py). Such a call opens no
// listener the Windows Firewall asks about, and what comes in has to be a
// reply to something sent out. In a DTLS-SRTP call the peer may take the
// client's part (setup:active) and start the handshake, and this side sends
// nothing until the keys are up: so, once the peer's SDP is in, one empty
// datagram (RFC 6263 4.1) goes from the RTP and the RTCP socket to the peer's
// addresses, and its retransmitted ClientHello then gets in. A pair that did
// not land on neighbouring ports is warned of, since the peer sends RTCP to
// the RTP port + 1.
//
// Nothing here runs with a port range: those calls are as baresip makes them.
#pragma once
#include <re.h>
#include <baresip.h>

namespace system_ports {
void on_event(bevent_ev ev, call *c);
} // namespace system_ports
