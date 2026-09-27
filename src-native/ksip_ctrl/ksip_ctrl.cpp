// The app's control connection to the engine: JSON commands and their
// answers in netstring frames over TCP on the loopback address, and the
// engine's events the same way back. Only the app may hold it: the engine is
// started with a secret (the environment variable KSIP_CONTROL_SECRET, set
// by the parent for this one process and never written to the profile or the
// log), and a connection may do nothing until its first frame is
// `{"command":"auth","params":"<secret>"}`. A connection that says anything
// else, or nothing within a few seconds, is closed. While one connection is
// authenticated, further connections are refused at the door, so that no
// other process on the machine can take the app's place or push it off; the
// app never reconnects to a running engine (a lost connection ends the
// process), so one connection per engine lifetime is what is wanted.
//
// Frames: `<length>:<json>,` as netstring says. A command is
// {"command","params","token"}; the answer is {"response":true,"ok","data",
// "token"}; an event is {"event":true, ...} as baresip encodes it.
//
// Threads: baresip's main thread only (libre's TCP callbacks, its timers,
// the events). Lifetime: module-static state; init() at module load,
// close() at unload; not re-initialised.
#define WIN32_LEAN_AND_MEAN
#include <cmath>
#include <algorithm>
#include <re.h>
#include <baresip.h>
#include <windows.h>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

namespace {
enum { kAuthGraceMs = 3000, kMaxFrame = 65536 };

struct Client {
    tcp_conn *tc = nullptr;
    // Bytes received and not yet framed.
    std::string in;
    bool authenticated = false;
    // Set once the connection is to be closed; the close itself happens from
    // a timer, outside libre's callbacks for this connection.
    bool closing = false;
    tmr timer{};
    ~Client() {
        tmr_cancel(&timer);
        mem_deref(tc);
    }
};

tcp_sock *listener = nullptr;
std::string secret;
std::vector<std::unique_ptr<Client>> clients;

Client *authenticated_client() {
    for (auto &c : clients)
        if (c->authenticated && !c->closing) return c.get();
    return nullptr;
}
void drop_now(void *arg) {
    auto c = static_cast<Client *>(arg);
    clients.erase(std::remove_if(clients.begin(), clients.end(), [c](const std::unique_ptr<Client> &p) { return p.get() == c; }), clients.end());
}
// Ends a connection at the next turn of the loop: never from inside one of
// its own callbacks.
void drop_later(Client *c) {
    if (c->closing) return;
    c->closing = true;
    tmr_start(&c->timer, 0, drop_now, c);
}

int print_to_string(const char *p, size_t size, void *arg) {
    static_cast<std::string *>(arg)->append(p, size);
    return 0;
}
int send_frame(tcp_conn *tc, const std::string &body) {
    mbuf *mb = mbuf_alloc(body.size() + 24);
    if (!mb) return ENOMEM;
    int err = mbuf_printf(mb, "%zu:", body.size());
    err |= mbuf_write_mem(mb, reinterpret_cast<const uint8_t *>(body.data()), body.size());
    err |= mbuf_write_u8(mb, ',');
    if (!err) {
        mb->pos = 0;
        err = tcp_send(tc, mb);
    }
    mem_deref(mb);
    return err;
}
std::string encode_response(int cmd_error, const std::string &data, const char *token) {
    odict *od = nullptr;
    if (odict_alloc(&od, 8)) return {};
    char m[256];
    odict_entry_add(od, "response", ODICT_BOOL, true);
    odict_entry_add(od, "ok", ODICT_BOOL, cmd_error == 0);
    if (cmd_error && data.empty()) odict_entry_add(od, "data", ODICT_STRING, str_error(cmd_error, m, sizeof(m)));
    else odict_entry_add(od, "data", ODICT_STRING, data.c_str());
    if (token) odict_entry_add(od, "token", ODICT_STRING, token);
    std::string out;
    re_printf pf = {print_to_string, &out};
    if (json_encode_odict(&pf, od)) out.clear();
    mem_deref(od);
    return out;
}

// One frame from an authenticated connection: the command runs, the answer
// goes back with the token.
void run_command(Client *c, odict *od) {
    const char *cmd = odict_string(od, "command"), *prm = odict_string(od, "params"), *tok = odict_string(od, "token");
    std::string line = cmd;
    if (prm && *prm) line += std::string(" ") + prm;
    std::string data;
    re_printf pf = {print_to_string, &data};
    int err = cmd_process_long(baresip_commands(), line.c_str(), line.size(), &pf, nullptr);
    if (err) warning("ksip_ctrl: command %s failed (%m)\n", cmd, err);
    auto body = encode_response(err, data, tok);
    if (!body.empty() && send_frame(c->tc, body)) warning("ksip_ctrl: failed to send the response\n");
}
// The first frame of a connection: the secret, or the door.
void authenticate(Client *c, odict *od) {
    const char *cmd = odict_string(od, "command"), *prm = odict_string(od, "params"), *tok = odict_string(od, "token");
    const bool right = cmd && prm && !secret.empty() && str_casecmp(cmd, "auth") == 0 && strlen(prm) == secret.size() && memcmp(prm, secret.data(), secret.size()) == 0;
    if (!right || authenticated_client()) {
        // The words are not repeated: whatever was sent is not the log's.
        info("ksip_ctrl: a connection did not authenticate and is closed\n");
        drop_later(c);
        return;
    }
    c->authenticated = true;
    tmr_cancel(&c->timer);
    info("ksip_ctrl: the control connection is authenticated\n");
    auto body = encode_response(0, "", tok);
    if (!body.empty()) (void)send_frame(c->tc, body);
}
void handle_frame(Client *c, const char *data, size_t len) {
    odict *od = nullptr;
    if (json_decode_odict(&od, 32, data, len, 16) || !odict_string(od, "command")) {
        mem_deref(od);
        if (!c->authenticated) drop_later(c);
        else warning("ksip_ctrl: a frame could not be read\n");
        return;
    }
    if (c->authenticated) run_command(c, od);
    else authenticate(c, od);
    mem_deref(od);
}
// Netstring framing over what has arrived so far.
void recv_handler(mbuf *mb, void *arg) {
    auto c = static_cast<Client *>(arg);
    if (c->closing) return;
    c->in.append(reinterpret_cast<const char *>(mbuf_buf(mb)), mbuf_get_left(mb));
    for (;;) {
        auto colon = c->in.find(':');
        if (colon == std::string::npos) {
            if (c->in.size() > 10) drop_later(c); // no length in sight: not a frame
            return;
        }
        if (colon == 0 || colon > 9 || c->in.find_first_not_of("0123456789") < colon) {
            drop_later(c);
            return;
        }
        size_t len = std::stoul(c->in.substr(0, colon));
        if (len > kMaxFrame) {
            drop_later(c);
            return;
        }
        if (c->in.size() < colon + 1 + len + 1) return; // the rest is still to come
        if (c->in[colon + 1 + len] != ',') {
            drop_later(c);
            return;
        }
        std::string frame = c->in.substr(colon + 1, len);
        c->in.erase(0, colon + 1 + len + 1);
        handle_frame(c, frame.data(), frame.size());
        if (c->closing) return;
    }
}
void close_handler(int, void *arg) {
    auto c = static_cast<Client *>(arg);
    if (c->authenticated) info("ksip_ctrl: the control connection closed\n");
    drop_later(c);
}
void grace_over(void *arg) {
    auto c = static_cast<Client *>(arg);
    info("ksip_ctrl: a connection said nothing in time and is closed\n");
    drop_later(c);
}
void conn_handler(const sa *, void *) {
    if (authenticated_client() || clients.size() >= 4) {
        // The app holds the connection; nobody takes its place.
        (void)tcp_reject(listener);
        return;
    }
    auto c = std::make_unique<Client>();
    if (tcp_accept(&c->tc, listener, nullptr, recv_handler, close_handler, c.get())) return;
    tmr_start(&c->timer, kAuthGraceMs, grace_over, c.get());
    clients.push_back(std::move(c));
}
// The engine's events, to the app.
void event_handler(bevent_ev, bevent *event, void *) {
    auto c = authenticated_client();
    if (!c) return;
    odict *od = nullptr;
    if (odict_alloc(&od, 8)) return;
    int err = odict_entry_add(od, "event", ODICT_BOOL, true);
    err |= bevent_odict_encode(od, event);
    std::string body;
    re_printf pf = {print_to_string, &body};
    if (!err) err = json_encode_odict(&pf, od);
    mem_deref(od);
    if (err || send_frame(c->tc, body)) warning("ksip_ctrl: failed to send an event\n");
}

int init() {
    char value[256] = {};
    DWORD n = GetEnvironmentVariableA("KSIP_CONTROL_SECRET", value, sizeof(value));
    if (n > 0 && n < sizeof(value)) secret.assign(value, n);
    if (secret.empty()) warning("ksip_ctrl: no control secret in the environment; every connection will be refused\n");
    sa laddr;
    if (conf_get_sa(conf_cur(), "ksip_ctrl_listen", &laddr)) sa_set_str(&laddr, "127.0.0.1", 0);
    int err = tcp_listen(&listener, &laddr, conn_handler, nullptr);
    if (err) {
        warning("ksip_ctrl: failed to listen on %J (%m)\n", &laddr, err);
        return err;
    }
    return bevent_register(event_handler, nullptr);
}
int close() {
    bevent_unregister(event_handler);
    clients.clear();
    listener = static_cast<tcp_sock *>(mem_deref(listener));
    secret.clear();
    return 0;
}
} // namespace

extern "C" const struct mod_export DECL_EXPORTS(ksip_ctrl) = {"ksip_ctrl", "application", init, close};
