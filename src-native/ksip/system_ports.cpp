// The way in for a call's RTP; see system_ports.h.
#include "system_ports.h"
#include <cstring>
#include <vector>

// baresip's own, declared in its src/core.h rather than its public header:
// the RTP socket of a stream.
extern "C" struct rtp_sock *stream_rtp_sock(const struct stream *strm);

namespace system_ports {
namespace {
// How often the calls are looked at for RTP not sent, and how long without
// any before the way in is opened again: well within the firewall's minute.
constexpr uint64_t kLookMs = 5000;
constexpr uint64_t kQuietMs = 20000;
// A call and what its stream had sent at the last look, and since when that
// has not changed.
struct Quiet {
    const call *c;
    uint32_t tx_packets;
    uint64_t since_ms;
};
std::vector<Quiet> g_quiet;
tmr g_look;

// "rtp_ports 0" in the configuration: what libre takes as the system's ports.
bool system_picked() {
    const config *c = conf_config();
    return c && !c->avt.rtp_ports.min && !c->avt.rtp_ports.max;
}
void send_empty(void *sock, const sa *to) {
    if (!sock || !to || !sa_isset(to, SA_ALL) || !sa_port(to)) return;
    mbuf *mb = mbuf_alloc(1);
    if (mb) (void)udp_send(static_cast<udp_sock *>(sock), to, mb);
    mem_deref(mb);
}
// The call's audio stream, its SDP and its RTP socket; false while it has none.
bool media_of(call *c, stream *&s, sdp_media *&m, struct rtp_sock *&rtp) {
    audio *a = c ? call_audio(c) : nullptr;
    s = a ? audio_strm(a) : nullptr;
    m = s ? stream_sdpmedia(s) : nullptr;
    rtp = s ? stream_rtp_sock(s) : nullptr;
    return m && rtp;
}
// One empty datagram from the RTP socket and one from the RTCP socket to the
// peer's media addresses, so that what the peer sends there gets in. A peer
// that gave no address (an old form of hold: 0.0.0.0, or port 0) is left alone.
void open_way_in(call *c, const char *why) {
    stream *s;
    sdp_media *m;
    struct rtp_sock *rtp;
    if (!media_of(c, s, m, rtp)) return;
    const sa *to = sdp_media_raddr(m);
    if (!to || !sa_isset(to, SA_ALL) || !sa_port(to)) return;
    send_empty(rtp_sock(rtp), to);
    sa rtcp;
    sdp_media_raddr_rtcp(m, &rtcp);
    send_empty(rtcp_sock(rtp), &rtcp);
    info("ksip: empty datagrams to the peer's RTP and RTCP keep the way in open (%s)\n", why);
}
// Every kLookMs: a call whose stream has sent nothing for kQuietMs has its
// way in opened again; a call up, or one answered early (a 183 with an
// announcement the peer only sends), whose peer gave a media address
// (open_way_in sees to that). What was sent is read off the stream's own
// count (the empty datagrams are not in it), and a call gone is forgotten.
void look(void *) {
    tmr_start(&g_look, kLookMs, look, nullptr);
    const uint64_t now = tmr_jiffies();
    std::vector<Quiet> kept;
    for (le *u = list_head(uag_list()); u; u = u->next) {
        for (le *l = list_head(ua_calls(static_cast<ua *>(u->data))); l; l = l->next) {
            call *c = static_cast<call *>(l->data);
            if (call_state(c) != CALL_STATE_ESTABLISHED && call_state(c) != CALL_STATE_EARLY) continue;
            stream *s;
            sdp_media *m;
            struct rtp_sock *rtp;
            if (!media_of(c, s, m, rtp)) continue;
            const uint32_t tx = stream_metric_get_tx_n_packets(s);
            Quiet q{c, tx, now};
            for (const Quiet &known : g_quiet) {
                if (known.c == c) {
                    q = known;
                    break;
                }
            }
            if (q.tx_packets != tx) {
                q.tx_packets = tx;
                q.since_ms = now;
            } else if (now - q.since_ms >= kQuietMs) {
                open_way_in(c, "no RTP sent for 20 s");
                q.since_ms = now;
            }
            kept.push_back(q);
        }
    }
    g_quiet.swap(kept);
}
} // namespace

void init() {
    tmr_init(&g_look);
    tmr_start(&g_look, kLookMs, look, nullptr);
}
void close() {
    tmr_cancel(&g_look);
    g_quiet.clear();
}
void on_event(bevent_ev ev, call *c) {
    if (ev != BEVENT_CALL_REMOTE_SDP || !c) return;
    stream *s;
    sdp_media *m;
    struct rtp_sock *rtp;
    if (!media_of(c, s, m, rtp)) return;
    // The direction as negotiated, from this side's point of view (libre
    // turns the peer's attribute round: its "sendonly" is "recvonly" here).
    const enum sdp_dir direction = sdp_media_dir(m);
    info("ksip: audio %s after the peer's SDP, the peer's RTP at %J\n", sdp_dir_name(direction), sdp_media_raddr(m));
    if (system_picked()) {
        sa rtcp_local;
        if (rtcp_sock(rtp) && !udp_local_get(static_cast<udp_sock *>(rtcp_sock(rtp)), &rtcp_local) &&
            sa_port(&rtcp_local) != sa_port(rtp_local(rtp)) + 1)
            warning("ksip: RTCP is on port %u, not next to RTP on %u; the SDP says so (a=rtcp), a peer that ignores it sends its RTCP next to RTP\n",
                    sa_port(&rtcp_local), sa_port(rtp_local(rtp)));
        const char *proto = sdp_media_proto(m);
        if (proto && strstr(proto, "TLS")) {
            send_empty(rtp_sock(rtp), sdp_media_raddr(m));
            sa rtcp;
            sdp_media_raddr_rtcp(m, &rtcp);
            send_empty(rtcp_sock(rtp), &rtcp);
        }
    }
    // This side only receives (the peer's offer or answer said it only
    // sends): no RTP goes out, and the way in is opened here, now.
    if (direction == SDP_RECVONLY) open_way_in(c, "the peer only sends");
}
} // namespace system_ports
