// The app's control connection to the engine: JSON commands and their
// answers in netstring frames over TCP on the loopback address, and the
// engine's events the same way back. (The engine's other link to the app,
// its stdout and stderr on the app's pipes, is stdio.cpp beside this.)
//
// The engine listens on nothing. The app holds a listening socket from
// before it starts the engine until the engine has connected, so there is no
// moment in which another process could take the port; it tells the engine
// the port (`ksip_ctrl_connect` in the config) and a secret made for this
// start (the environment variable KSIP_CONTROL_SECRET, set for this one
// process and never written to the profile or the log). The engine connects
// out and its first frame names the secret:
// `{"hello":"ksip_ctrl","secret":"<secret>"}`. The app takes the connection
// that says it and closes any other; the secret only ever travels to the
// socket the app has held throughout. One connection per engine lifetime:
// when it ends, the engine quits, as it would on the app's `quit`, so an
// engine never outlives the app that controls it.
//
// Frames: `<length>:<json>,` as netstring says. A command is
// {"command","params","token"}; the answer is {"response":true,"ok","data",
// "token"}; an event is {"event":true, ...} as baresip encodes it.
//
// Threads: baresip's main thread only (libre's TCP callbacks, the events).
// Lifetime: module-static state; init() at module load, close() at unload;
// not re-initialised.
#define WIN32_LEAN_AND_MEAN
#include <re.h>
#include <baresip.h>
#include <windows.h>
#include <cstring>
#include <string>
#include <cerrno>
#include <cstdint>

namespace {
enum { kMaxFrame = 65536 };

tcp_conn *conn = nullptr;
bool established = false;
// Bytes received and not yet framed.
std::string in;
std::string secret;
// Set once the connection has ended: the engine is on its way out.
bool ended = false;
// Lets go of the connection at the next turn of the loop, never from inside
// one of its own callbacks.
tmr release_timer;

int print_to_string(const char *p, size_t size, void *arg) {
    static_cast<std::string *>(arg)->append(p, size);
    return 0;
}
// odict's hash buckets and how deep a command's JSON may nest; the room a
// netstring takes around its body (the length, the colon and the comma); the
// text of an error, and of the secret the app hands over.
constexpr uint32_t kDictBuckets = 16;
constexpr unsigned kJsonDepth = 16;
constexpr size_t kFrameOverhead = 24, kErrorTextSize = 256, kSecretSize = 256;
// A frame's length has at most this many digits (kMaxFrame has seven), and
// a frame has to show its colon by then.
constexpr size_t kMaxLengthDigits = 9;
int send_frame(const std::string &body) {
    if (!conn || !established) return ENOTCONN;
    mbuf *mb = mbuf_alloc(body.size() + kFrameOverhead);
    if (!mb) return ENOMEM;
    int err = mbuf_printf(mb, "%zu:", body.size());
    err |= mbuf_write_mem(mb, reinterpret_cast<const uint8_t *>(body.data()), body.size());
    err |= mbuf_write_u8(mb, ',');
    if (!err) {
        mb->pos = 0;
        err = tcp_send(conn, mb);
    }
    mem_deref(mb);
    return err;
}
std::string encode_response(int cmd_error, const std::string &data, const char *token) {
    odict *od = nullptr;
    if (odict_alloc(&od, kDictBuckets)) return {};
    char m[kErrorTextSize];
    odict_entry_add(od, "response", ODICT_BOOL, 1);
    odict_entry_add(od, "ok", ODICT_BOOL, static_cast<int>(cmd_error == 0));
    if (cmd_error && data.empty()) odict_entry_add(od, "data", ODICT_STRING, str_error(cmd_error, m, sizeof(m)));
    else odict_entry_add(od, "data", ODICT_STRING, data.c_str());
    if (token) odict_entry_add(od, "token", ODICT_STRING, token);
    std::string out;
    re_printf pf = {print_to_string, &out};
    if (json_encode_odict(&pf, od)) out.clear();
    mem_deref(od);
    return out;
}
void release(void *) { conn = static_cast<tcp_conn *>(mem_deref(conn)); }
// The engine's controller is gone: the engine goes too.
void end(const char *why) {
    if (ended) return;
    ended = true;
    info("ksip_ctrl: %s; the engine quits\n", why);
    established = false;
    tmr_start(&release_timer, 0, release, nullptr);
    ua_stop_all(false);
}

// One frame: the command runs, the answer goes back with the token.
void run_command(const char *data, size_t len) {
    odict *od = nullptr;
    if (json_decode_odict(&od, kDictBuckets, data, len, kJsonDepth) || !odict_string(od, "command")) {
        mem_deref(od);
        warning("ksip_ctrl: a frame could not be read\n");
        return;
    }
    const char *cmd = odict_string(od, "command"), *prm = odict_string(od, "params"), *tok = odict_string(od, "token");
    std::string line = cmd;
    if (prm && *prm) line += std::string(" ") + prm;
    std::string answer;
    re_printf pf = {print_to_string, &answer};
    const int err = cmd_process_long(baresip_commands(), line.c_str(), line.size(), &pf, nullptr);
    if (err) warning("ksip_ctrl: command %s failed (%m)\n", cmd, err);
    const auto body = encode_response(err, answer, tok);
    mem_deref(od);
    if (!body.empty() && send_frame(body)) warning("ksip_ctrl: failed to send the response\n");
}
// Netstring framing over what has arrived so far.
void recv_handler(mbuf *mb, void *) {
    if (ended) return;
    in.append(reinterpret_cast<const char *>(mbuf_buf(mb)), mbuf_get_left(mb));
    for (;;) {
        const auto colon = in.find(':');
        if (colon == std::string::npos) {
            if (in.size() > kMaxLengthDigits + 1) end("the app sent something that is not a frame");
            return;
        }
        if (colon == 0 || colon > kMaxLengthDigits || in.find_first_not_of("0123456789") < colon) return end("the app sent something that is not a frame");
        const size_t len = std::stoul(in.substr(0, colon));
        if (len > kMaxFrame) return end("the app sent a frame too long");
        if (in.size() < colon + 1 + len + 1) return; // the rest is still to come
        if (in[colon + 1 + len] != ',') return end("the app sent something that is not a frame");
        std::string frame = in.substr(colon + 1, len);
        in.erase(0, colon + 1 + len + 1);
        run_command(frame.data(), frame.size());
        if (ended) return;
    }
}
// Connected: the first word is the secret.
void estab_handler(void *) {
    established = true;
    odict *od = nullptr;
    if (odict_alloc(&od, 4)) return end("out of memory");
    odict_entry_add(od, "hello", ODICT_STRING, "ksip_ctrl");
    odict_entry_add(od, "secret", ODICT_STRING, secret.c_str());
    std::string body;
    re_printf pf = {print_to_string, &body};
    const int err = json_encode_odict(&pf, od);
    mem_deref(od);
    if (err || send_frame(body)) return end("the greeting could not be sent");
    info("ksip_ctrl: connected to the app\n");
}
void close_handler(int err, void *) {
    if (established) end("the control connection closed");
    else {
        warning("ksip_ctrl: could not connect to the app (%m)\n", err);
        end("no control connection");
    }
}
// The engine's events, to the app.
void event_handler(bevent_ev, bevent *event, void *) {
    if (!established || ended) return;
    odict *od = nullptr;
    if (odict_alloc(&od, kDictBuckets)) return;
    int err = odict_entry_add(od, "event", ODICT_BOOL, 1);
    err |= bevent_odict_encode(od, event);
    std::string body;
    re_printf pf = {print_to_string, &body};
    if (!err) err = json_encode_odict(&pf, od);
    mem_deref(od);
    if (err || send_frame(body)) warning("ksip_ctrl: failed to send an event\n");
}

// The engine cannot be controlled: it ends as soon as its loop runs, rather
// than go on uncontrolled. baresip carries on past a module that fails to
// load, so returning the error alone would not stop it.
// Its own timer: libre closes a module whose init failed, and close() must
// not cancel this one.
tmr quit_timer;
const char *quit_reason = "";
void quit_uncontrolled(void *) {
    warning("ksip_ctrl: %s; the engine quits\n", quit_reason);
    ended = true;
    ua_stop_all(false);
}
int refuse(const char *why, int err) {
    quit_reason = why;
    tmr_start(&quit_timer, 0, quit_uncontrolled, nullptr);
    return err;
}
int init() {
    tmr_init(&release_timer);
    tmr_init(&quit_timer);
    char value[kSecretSize] = {};
    const DWORD n = GetEnvironmentVariableA("KSIP_CONTROL_SECRET", value, sizeof(value));
    if (n > 0 && n < sizeof(value)) secret.assign(value, n);
    SecureZeroMemory(value, sizeof(value));
    if (secret.empty()) return refuse("no control secret in the environment", EINVAL);
    sa peer;
    if (conf_get_sa(conf_cur(), "ksip_ctrl_connect", &peer) || !sa_is_loopback(&peer))
        return refuse("ksip_ctrl_connect must name the app's loopback address and port", EINVAL);
    int err = bevent_register(event_handler, nullptr);
    if (err) return refuse("the engine's events could not be followed", err);
    err = tcp_connect(&conn, &peer, estab_handler, recv_handler, close_handler, nullptr);
    if (err) return refuse("the connection to the app could not be started", err);
    return 0;
}
int close() {
    tmr_cancel(&release_timer);
    bevent_unregister(event_handler);
    established = false;
    conn = static_cast<tcp_conn *>(mem_deref(conn));
    if (!secret.empty()) SecureZeroMemory(&secret[0], secret.size());
    secret.clear();
    return 0;
}
} // namespace

extern "C" const struct mod_export DECL_EXPORTS(ksip_ctrl) = {"ksip_ctrl", "application", init, close};
