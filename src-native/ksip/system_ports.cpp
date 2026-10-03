// Calls on system-picked ports; see system_ports.h.
#include "system_ports.h"
#include <cstring>

// baresip's own, declared in its src/core.h rather than its public header:
// the RTP socket of a stream.
extern "C" struct rtp_sock *stream_rtp_sock(const struct stream *strm);

namespace system_ports {
namespace {
// "rtp_ports 0" in the configuration: what libre takes as the system's ports.
bool system_picked() {
    const config *c = conf_config();
    return c && !c->avt.rtp_ports.min && !c->avt.rtp_ports.max;
}
void send_empty(void *sock, const sa *to) {
    if (!sock || !to || !sa_isset(to, SA_ALL)) return;
    mbuf *mb = mbuf_alloc(1);
    if (mb) (void)udp_send(static_cast<udp_sock *>(sock), to, mb);
    mem_deref(mb);
}
} // namespace

void on_event(bevent_ev ev, call *c) {
    if (ev != BEVENT_CALL_REMOTE_SDP || !c || !system_picked()) return;
    audio *a = call_audio(c);
    stream *s = a ? audio_strm(a) : nullptr;
    sdp_media *m = s ? stream_sdpmedia(s) : nullptr;
    struct rtp_sock *rtp = s ? stream_rtp_sock(s) : nullptr;
    if (!m || !rtp) return;
    sa rtcp_local;
    if (rtcp_sock(rtp) && !udp_local_get(static_cast<udp_sock *>(rtcp_sock(rtp)), &rtcp_local) &&
        sa_port(&rtcp_local) != sa_port(rtp_local(rtp)) + 1)
        warning("ksip: RTCP is on port %u, not next to RTP on %u; the peer's RTCP may not arrive\n", sa_port(&rtcp_local),
                sa_port(rtp_local(rtp)));
    const char *proto = sdp_media_proto(m);
    if (!proto || !strstr(proto, "TLS")) return;
    send_empty(rtp_sock(rtp), sdp_media_raddr(m));
    sa rtcp;
    sdp_media_raddr_rtcp(m, &rtcp);
    send_empty(rtcp_sock(rtp), &rtcp);
}
} // namespace system_ports
