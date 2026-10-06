// The module's reading and writing that touches libre or baresip types: a
// display name out of a `pl`, the SIP log, the JSON of the app's commands,
// and the audio statistics of the state reply. The plain-text part lives in
// ksip_text.h and is reachable through this namespace as well.
//
// Threads: called on baresip's main thread (commands, events, SIP trace).
#pragma once
#include <cmath>
#include <algorithm>
#include <re.h>
#include <baresip.h>
#include <cstdint>
#include <string>
#include "ksip_audio_bridge.h"
#include "ksip_text.h"

namespace ksip_io {
// odict's hash buckets: the dictionaries here hold a handful of entries, so
// one nominal size does for all of them. And how deep a command's JSON may nest.
constexpr uint32_t DICT_BUCKETS = 16;
constexpr unsigned JSON_DEPTH = 4;
// The SIP status codes this module reads or answers with, by name.
namespace sip_status {
constexpr uint16_t OK = 200, LOWEST_FAILURE = 300, FORBIDDEN = 403, NOT_FOUND = 404, METHOD_NOT_ALLOWED = 405, GONE = 410,
                   UNSUPPORTED_URI_SCHEME = 416, CALL_DOES_NOT_EXIST = 481, BUSY_HERE = 486, BAD_EVENT = 489,
                   NOT_IMPLEMENTED = 501, DECLINE = 603, DOES_NOT_EXIST_ANYWHERE = 604;
} // namespace sip_status
using ksip_text::address_ok;
using ksip_text::codec_list;
using ksip_text::escape_user;
using ksip_text::parse_audio_devices;
using ksip_text::parse_watch_list;
using ksip_text::read_dialog_info;
using ksip_text::scrubbed_sip_lines;
using ksip_text::sip_header;
using ksip_text::sip_uri;
using ksip_text::token;
// A display name as the window shows it: without the quotes a header puts
// around it, and empty when there is none.
std::string display_name(const pl &name);
// Writes the scrubbed lines to the log, marked as sent or received.
void log_sip_message(bool tx, const uint8_t *packet, size_t length);
// What the app asks of a call: the JSON of ksip_action read into its parts.
// An operation on a call; `mode` is how a DTMF digit is sent (rtp, info or
// inband), the other operations leave it empty.
struct ActionRequest {
    std::string op, id, value, mode;
};
bool parse_action(const char *prm, ActionRequest &request);
// The echo canceller's statistics as the state reply carries them.
void add_audio_stats(odict *od, const ksip_audio_stats &stats);
} // namespace ksip_io
