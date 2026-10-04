// The way in for a call's RTP when the Windows Firewall has no rule for it:
// with the RTP and RTCP ports picked by the system ("rtp_ports 0", the
// libre change in scripts/build/patch-baresip.py) the call opens no listener
// the firewall asks about, and in any case without a rule, what comes in has
// to be a reply to something sent out from the same socket to the same
// peer address and port.
//
// Three things see to that. In a DTLS-SRTP call the peer may take the
// client's part (setup:active) and start the handshake, and this side sends
// nothing until the keys are up: so, once the peer's SDP is in, one empty
// datagram (RFC 6263 4.1) goes from the RTP and the RTCP socket to the
// peer's addresses, and its retransmitted ClientHello then gets in. When the
// peer's SDP says it only sends (hold music, an announcement, early media:
// a=sendonly), baresip sends no RTP and the way in would never open, so the
// same empty datagrams go out then, at once. And while a call sends no RTP
// for twenty seconds, for whatever reason (the direction, hold on this
// side), they go out again, since the firewall forgets a way in that is not
// used in about a minute (Windows' ALE stateful filtering, default UDP
// timeout 60 s). A pair of ports that did not land next to each other is
// warned of, since the peer sends RTCP to the RTP port + 1.
//
// The empty datagram is nothing to the peer: RTP and SRTP drop a packet
// shorter than a header.
#pragma once
#include <re.h>
#include <baresip.h>

namespace system_ports {
void init();
void close();
void on_event(bevent_ev ev, call *c);
} // namespace system_ports
