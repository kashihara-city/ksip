// The module's reading of plain text, with nothing of libre or baresip in
// it: SIP header values and the lines a SIP message may show in the log,
// the dialog-info bodies of the parking subscriptions, numbers and URIs,
// and the lists the app sends. Kept apart so that it can be tried without
// an engine (scripts/test/audio-module.ps1 builds it into the unit tests).
//
// Threads: pure functions, safe from any thread.
#pragma once
#include <array>
#include <cstddef>
#include <cstdint>
#include <string>
#include <utility>
#include <vector>

namespace ksip_text {
// A button may name a full SIP URI instead of a number; it is passed on as it
// is, and only has to be one line of visible ASCII.
bool sip_uri(const std::string &s);
bool address_ok(const std::string &s);
// A token of the characters SIP allows in a user or host, plus `extra`.
bool token(const char *s, const char *extra);
// The value of a header in a SIP message, or empty. The name is matched
// without regard to case, and the white space HCOLON allows before the colon
// (RFC 3261 25.1) is skipped. A value folded over several lines (RFC 3261
// 7.3.1) comes as one, each fold and the white space around it one space.
std::string sip_header(const uint8_t *packet, size_t length, const char *name);
// What identifies the transaction of a request as it was sent: its Call-ID,
// its CSeq (number and method, spaces evened out) and the branch of its top
// Via. call_id is empty when the packet is not a request of that method.
struct TransactionIds {
    std::string call_id, cseq, branch;
};
TransactionIds request_ids(const uint8_t *packet, size_t length, const char *method);
// Whether a packet is a 2xx answer to that request: the same Call-ID, CSeq
// and top Via branch (the branch is new for each transaction, so an answer
// that did not see the request cannot carry it). The compact names of
// Call-ID (i) and Via (v) are read too.
bool is_success_answer(const TransactionIds &request, const uint8_t *packet, size_t length);
// The lines of a SIP message as the log may show them: the digest headers
// (Authorization, Proxy-Authorization, WWW-Authenticate, Proxy-Authenticate),
// however cased and spaced, are replaced together with their folded
// continuation lines, and so are the SRTP keys of a=crypto.
std::vector<std::string> scrubbed_sip_lines(const uint8_t *packet, size_t length);
// A dialog-info body (RFC 4235) as far as the parking state needs it: whether
// it could be read as one, whether it is a full or a partial report, and the
// (id, state) of each dialog. Attributes may use either quote and spaces
// around `=`; a namespace prefix, a self-closing element or unusual spacing
// does not change the reading. The body is checked as a whole before any
// of it counts: every element must close in order, dialog-info must say
// whether it is full or partial, every dialog must carry an id and one of
// the states RFC 4235 names. Anything short of that, a body cut off before
// its closing tag included, reads as nothing (readable=false), so that the
// state kept from the last good report stays; it never reads as "free".
struct DialogInfo {
    bool readable = false;
    bool partial = false;
    std::vector<std::pair<std::string, std::string>> dialogs;
};
DialogInfo read_dialog_info(const std::string &body);
// The codecs the app chose, in its order, as the account parameter names
// them; a name this build does not know is skipped, and none at all means
// every codec in the usual order.
std::string codec_list(const std::string &names);
// A user part as it goes into a URI: `#` has to be escaped.
std::string escape_user(const std::string &value);
// How many numbers the buttons can watch: one for each custom button
// (CustomButton::COUNT in the app's settings.rs).
constexpr size_t WATCH_COUNT = 54;
// The numbers the buttons watch, comma-separated, up to WATCH_COUNT; an
// empty one is a slot nobody watches. False for a number that is not a
// number or URI, for too many, or for one named twice.
bool parse_watch_list(const char *prm, std::array<std::string, WATCH_COUNT> &values);
// The audio devices command: microphone and speaker endpoint ids, comma-separated.
bool parse_audio_devices(const char *prm, size_t limit, std::string &microphone, std::string &speaker);
} // namespace ksip_text
