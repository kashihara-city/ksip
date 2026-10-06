// The module's reading of plain text; see ksip_text.h.
#include "ksip_text.h"
#include <cctype>
#include <cstring>
#include <vector>
#include <array>
#include <cstdint>
#include <string>
#include <string_view>
#include <utility>

namespace ksip_text {
namespace {
std::string lower(std::string s) {
    for (auto &ch : s) ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    return s;
}
std::string trim(const std::string &s) {
    const auto first = s.find_first_not_of(" \t\r\n"), last = s.find_last_not_of(" \t\r\n");
    return first == std::string::npos ? std::string() : s.substr(first, last - first + 1);
}
// The name in front of a header's colon, in lower case and without the white
// space HCOLON allows around it (RFC 3261 25.1: `HCOLON = *( SP / HTAB ) ":" SWS`).
std::string header_name(const std::string &line, size_t colon) {
    size_t end = colon;
    while (end > 0 && (line[end - 1] == ' ' || line[end - 1] == '\t')) --end;
    return lower(line.substr(0, end));
}
} // namespace

// The limits of what is taken as an address, a host name or a watched
// number, and the ASCII the three may hold.
constexpr size_t MAX_ADDRESS = 200, MAX_HOST = 253, MAX_NUMBER = 30;
constexpr unsigned char FIRST_GRAPHIC = 0x21, LAST_GRAPHIC = 0x7e, SPACE = 0x20, DEL = 0x7f, ASCII_END = 128;
bool sip_uri(const std::string &s) {
    constexpr std::string_view SIPS = "sips:";
    const std::string head = lower(s.substr(0, SIPS.size()));
    return head.starts_with("sip:") || head.starts_with(SIPS);
}
bool address_ok(const std::string &s) {
    if (s.empty() || s.size() > MAX_ADDRESS) return false;
    for (const char c : s)
        if (const auto ch = static_cast<unsigned char>(c); ch < FIRST_GRAPHIC || ch > LAST_GRAPHIC) return false;
    return true;
}
bool token(const char *s, const char *extra) {
    if (!s || !*s || strlen(s) > MAX_HOST) return false;
    for (; *s; ++s)
        if (!(static_cast<unsigned char>(*s) < ASCII_END && (isalnum(static_cast<unsigned char>(*s)) || strchr(extra, *s)))) return false;
    return true;
}
std::string sip_header(const uint8_t *packet, size_t length, const char *name) {
    const std::string wanted = lower(name);
    // The lines of the head, each without its CRLF, up to the blank line.
    std::vector<std::string> lines;
    size_t pos = 0;
    while (pos < length) {
        size_t end = pos;
        while (end + 1 < length && !(packet[end] == '\r' && packet[end + 1] == '\n')) ++end;
        if (end == pos) break;
        lines.emplace_back(reinterpret_cast<const char *>(packet + pos), end - pos);
        if (end + 1 >= length) break;
        pos = end + 2;
    }
    const auto folded = [](const std::string &line) { return !line.empty() && (line[0] == ' ' || line[0] == '\t'); };
    // The first line (the request or status line) and the continuation lines
    // of a header are never taken for a header of their own.
    for (size_t i = 1; i < lines.size(); ++i) {
        if (folded(lines[i])) continue;
        const auto colon = lines[i].find(':');
        if (colon == std::string::npos || header_name(lines[i], colon) != wanted) continue;
        // A value folded over several lines (RFC 3261 7.3.1): each line that
        // starts with white space continues it, and the fold is one space.
        std::string value = lines[i].substr(colon + 1);
        for (size_t j = i + 1; j < lines.size() && folded(lines[j]); ++j) value += ' ' + lines[j];
        size_t begin = 0;
        while (begin < value.size() && (value[begin] == ' ' || value[begin] == '\t')) ++begin;
        size_t stop = value.size();
        while (stop > begin && (value[stop - 1] == ' ' || value[stop - 1] == '\t')) --stop;
        std::string out;
        // The white space of the fold, and around it, reads as one space.
        for (size_t k = begin; k < stop; ++k) {
            const bool space = value[k] == ' ' || value[k] == '\t';
            if (space && !out.empty() && out.back() == ' ') continue;
            out += space ? ' ' : value[k];
        }
        return out;
    }
    return {};
}
namespace {
// A header by its name or its compact form (RFC 3261 7.3.3).
std::string header_or_compact(const uint8_t *packet, size_t length, const char *name, const char *compact) {
    const auto value = sip_header(packet, length, name);
    return value.empty() ? sip_header(packet, length, compact) : value;
}
std::string evened(const std::string &s) {
    std::string out;
    for (const char c : s) {
        if (c == ' ' || c == '\t') {
            if (!out.empty() && out.back() != ' ') out += ' ';
        } else {
            out += c;
        }
    }
    while (!out.empty() && out.back() == ' ') out.pop_back();
    return out;
}
// The branch parameter of a Via value (the first Via, when several are on one line).
std::string via_branch(const std::string &via) {
    const std::string first = via.substr(0, via.find(','));
    const std::string low = lower(first);
    constexpr std::string_view BRANCH = "branch";
    size_t at = 0;
    while ((at = low.find(BRANCH, at)) != std::string::npos) {
        size_t eq = at + BRANCH.size();
        while (eq < low.size() && (low[eq] == ' ' || low[eq] == '\t')) ++eq;
        if (at > 0 && (low[at - 1] == ';' || low[at - 1] == ' ' || low[at - 1] == '\t') && eq < low.size() && low[eq] == '=') {
            size_t begin = eq + 1;
            while (begin < first.size() && (first[begin] == ' ' || first[begin] == '\t')) ++begin;
            size_t end = begin;
            while (end < first.size() && first[end] != ';' && first[end] != ' ' && first[end] != '\t') ++end;
            return first.substr(begin, end - begin);
        }
        at += BRANCH.size();
    }
    return {};
}
} // namespace
TransactionIds request_ids(const uint8_t *packet, size_t length, const char *method) {
    const size_t n = strlen(method);
    if (!packet || length <= n || memcmp(packet, method, n) != 0 || packet[n] != ' ') return {};
    TransactionIds ids{header_or_compact(packet, length, "Call-ID", "i"), evened(sip_header(packet, length, "CSeq")),
                       via_branch(header_or_compact(packet, length, "Via", "v"))};
    if (ids.cseq.empty() || ids.branch.empty()) return {};
    return ids;
}
bool is_success_answer(const TransactionIds &request, const uint8_t *packet, size_t length) {
    constexpr std::string_view SUCCESS_LINE = "SIP/2.0 2";
    if (request.call_id.empty() || !packet || length < SUCCESS_LINE.size() || memcmp(packet, SUCCESS_LINE.data(), SUCCESS_LINE.size()) != 0) return false;
    return header_or_compact(packet, length, "Call-ID", "i") == request.call_id &&
           evened(sip_header(packet, length, "CSeq")) == request.cseq &&
           via_branch(header_or_compact(packet, length, "Via", "v")) == request.branch;
}
std::vector<std::string> scrubbed_sip_lines(const uint8_t *packet, size_t length) {
    // Secrets never reach the log: digest headers are replaced before printing,
    // together with any folded continuation lines of theirs (RFC 3261 7.3.1),
    // and so are the SRTP keys an SDP body carries in a=crypto (RFC 4568),
    // which SDES and OSRTP put in the signalling. The file this goes to
    // outlives the call, and a key in it would unlock a captured recording.
    static const char *secrets[] = {"authorization", "proxy-authorization", "www-authenticate", "proxy-authenticate"};
    const std::string text(reinterpret_cast<const char *>(packet), length);
    std::vector<std::string> lines;
    bool hiding = false;
    for (size_t pos = 0; pos < text.size();) {
        size_t end = text.find("\r\n", pos);
        if (end == std::string::npos) end = text.size();
        std::string line = text.substr(pos, end - pos);
        pos = end + 2;
        if (!line.empty() && (line[0] == ' ' || line[0] == '\t')) {
            if (hiding) continue;
        } else hiding = false;
        const auto colon = line.find(':');
        if (colon != std::string::npos) {
            const std::string name = header_name(line, colon);
            for (auto secret : secrets)
                if (name == secret) {
                    line = line.substr(0, colon) + ": ***";
                    hiding = true;
                    break;
                }
        }
        if (line.starts_with("a=crypto:")) {
            const auto key = line.find(" inline:");
            if (key != std::string::npos) line = line.substr(0, key) + " inline:***";
        }
        if (!line.empty()) lines.push_back(line);
    }
    return lines;
}
DialogInfo read_dialog_info(const std::string &body) {
    DialogInfo info;
    // An attribute's value, with either quote and any spacing around `=`.
    const auto attribute = [&](const std::string &attrs, const char *name) {
        const std::string wanted = lower(name);
        size_t pos = 0;
        while (pos < attrs.size()) {
            const size_t start = attrs.find_first_not_of(" \t\r\n", pos);
            if (start == std::string::npos) break;
            const size_t eq = attrs.find('=', start);
            if (eq == std::string::npos) break;
            const std::string key = lower(trim(attrs.substr(start, eq - start)));
            const size_t quote = attrs.find_first_not_of(" \t\r\n", eq + 1);
            if (quote == std::string::npos || (attrs[quote] != '"' && attrs[quote] != '\'')) break;
            const size_t close = attrs.find(attrs[quote], quote + 1);
            if (close == std::string::npos) break;
            if (key == wanted) return attrs.substr(quote + 1, close - quote - 1);
            pos = close + 1;
        }
        return std::string();
    };
    // Nothing counts until the whole body has passed: what is gathered here
    // becomes the reading only at the end.
    const auto nothing = [&] { return DialogInfo{}; };
    const auto known_state = [](const std::string &s) {
        return s == "trying" || s == "proceeding" || s == "early" || s == "confirmed" || s == "terminated";
    };
    std::vector<std::string> open;
    std::string dialog_id;
    bool dialog_has_state = false, seen_info = false, closed_info = false;
    for (size_t pos = body.find('<'); pos != std::string::npos; pos = body.find('<', pos)) {
        const auto end = body.find('>', pos);
        if (end == std::string::npos) return nothing();
        std::string tag = body.substr(pos + 1, end - pos - 1);
        pos = end + 1;
        if (tag.empty()) return nothing();
        if (tag[0] == '?' || tag[0] == '!') continue;
        if (closed_info) return nothing(); // an element after the report's end
        const bool closing = tag[0] == '/';
        if (closing) tag.erase(0, 1);
        tag = trim(tag);
        const bool self_closing = !closing && !tag.empty() && tag.back() == '/';
        if (self_closing) {
            tag.pop_back();
            tag = trim(tag);
        }
        const auto space = tag.find_first_of(" \t\r\n");
        std::string name = lower(tag.substr(0, space)), attrs = space == std::string::npos ? std::string() : tag.substr(space);
        const auto colon = name.find(':');
        if (colon != std::string::npos) name.erase(0, colon + 1);
        if (name.empty()) return nothing();
        if (closing) {
            // Every element closes in the order it opened.
            if (open.empty() || open.back() != name) return nothing();
            open.pop_back();
            if (name == "dialog") {
                if (!dialog_has_state) return nothing();
            } else if (name == "dialog-info") closed_info = true;
            continue;
        }
        if (open.empty()) {
            // The root is dialog-info, once, and says whether it is full or partial.
            if (name != "dialog-info" || seen_info) return nothing();
            seen_info = true;
            const std::string state = lower(attribute(attrs, "state"));
            if (state != "full" && state != "partial") return nothing();
            info.partial = state == "partial";
            if (self_closing) closed_info = true;
            else open.push_back(name);
            continue;
        }
        if (name == "dialog") {
            // A dialog sits right under dialog-info, with an id and a state.
            if (open.back() != "dialog-info" || self_closing) return nothing();
            dialog_id = attribute(attrs, "id");
            if (dialog_id.empty()) return nothing();
            dialog_has_state = false;
            open.push_back(name);
        } else if (name == "state" && open.back() == "dialog") {
            if (self_closing || dialog_has_state) return nothing();
            const auto text_end = body.find('<', pos);
            if (text_end == std::string::npos) return nothing();
            const std::string text = lower(trim(body.substr(pos, text_end - pos)));
            if (!known_state(text)) return nothing();
            info.dialogs.emplace_back(dialog_id, text);
            dialog_has_state = true;
            open.push_back(name);
        } else if (!self_closing) {
            // Other elements (local, remote, their children) are passed over,
            // but they too must close in order.
            open.push_back(name);
        }
    }
    if (!seen_info || !closed_info || !open.empty()) return nothing();
    info.readable = true;
    return info;
}
std::string codec_list(const std::string &names) {
    static const std::pair<const char *, const char *> known[] = {
        {"opus", "opus/48000/1"}, {"G722", "G722/16000/1"}, {"PCMU", "PCMU/8000/1"}, {"PCMA", "PCMA/8000/1"}};
    std::string codecs;
    for (size_t start = 0; start <= names.size();) {
        const auto end = names.find(',', start);
        const std::string name = names.substr(start, end == std::string::npos ? std::string::npos : end - start);
        for (auto &entry : known)
            if (name == entry.first && codecs.find(entry.second) == std::string::npos) codecs += (codecs.empty() ? "" : ",") + std::string(entry.second);
        if (end == std::string::npos) break;
        start = end + 1;
    }
    if (codecs.empty()) codecs = "opus/48000/1,G722/16000/1,PCMU/8000/1,PCMA/8000/1";
    return codecs;
}
std::string escape_user(const std::string &value) {
    std::string user;
    for (const char ch : value) user += ch == '#' ? "%23" : std::string(1, ch);
    return user;
}
bool parse_watch_list(const char *prm, std::array<std::string, WATCH_COUNT> &values) {
    if (!prm || !*prm) return false;
    const std::string text = prm;
    size_t start = 0, count = 0;
    for (;;) {
        if (count == values.size()) return false;
        const auto end = text.find(',', start);
        values[count] = text.substr(start, end == std::string::npos ? end : end - start);
        if (!values[count].empty() &&
            !(sip_uri(values[count]) ? address_ok(values[count]) : (values[count].size() <= MAX_NUMBER && token(values[count].c_str(), "*#+"))))
            return false;
        ++count;
        if (end == std::string::npos) break;
        start = end + 1;
    }
    for (size_t i = 0; i < values.size(); ++i)
        for (size_t j = i + 1; j < values.size(); ++j)
            if (!values[i].empty() && values[i] == values[j]) return false;
    return true;
}
bool parse_audio_devices(const char *prm, size_t limit, std::string &microphone, std::string &speaker) {
    if (!prm || !*prm) return false;
    const std::string text = prm;
    const auto comma = text.find(',');
    if (comma == std::string::npos) return false;
    microphone = text.substr(0, comma);
    speaker = text.substr(comma + 1);
    for (const auto &device : {microphone, speaker}) {
        if (device.empty() || device.size() >= limit) return false;
        for (const char c : device)
            if (const auto ch = static_cast<unsigned char>(c); ch < SPACE || ch == DEL || ch == ',') return false;
    }
    return true;
}
} // namespace ksip_text
