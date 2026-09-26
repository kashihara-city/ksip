// The module's reading and writing of text; see ksip_io.h.
#define WIN32_LEAN_AND_MEAN
#include "ksip_io.h"
#include <cctype>
#include <cstring>

namespace ksip_io {
namespace {
std::string lower(std::string s) {
    for (auto &ch : s) ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    return s;
}
std::string trim(std::string s) {
    auto first = s.find_first_not_of(" \t\r\n"), last = s.find_last_not_of(" \t\r\n");
    return first == std::string::npos ? std::string() : s.substr(first, last - first + 1);
}
} // namespace

std::string display_name(const pl &name) {
    if (!pl_isset(&name)) return "";
    std::string s(name.p, name.l);
    while (!s.empty() && (s.back() == ' ' || s.back() == '\t')) s.pop_back();
    if (s.size() >= 2 && s.front() == '"' && s.back() == '"') s = s.substr(1, s.size() - 2);
    return s;
}
bool sip_uri(const std::string &s) {
    std::string head = lower(s.substr(0, 5));
    return head.rfind("sip:", 0) == 0 || head.rfind("sips:", 0) == 0;
}
bool address_ok(const std::string &s) {
    if (s.empty() || s.size() > 200) return false;
    for (unsigned char ch : s) if (ch < 0x21 || ch > 0x7e) return false;
    return true;
}
bool token(const char *s, const char *extra) {
    if (!s || !*s || strlen(s) > 253) return false;
    for (; *s; ++s)
        if (!(static_cast<unsigned char>(*s) < 128 && (isalnum(static_cast<unsigned char>(*s)) || strchr(extra, *s)))) return false;
    return true;
}
std::string sip_header(const uint8_t *packet, size_t length, const char *name) {
    const size_t name_length = strlen(name);
    size_t pos = 0;
    while (pos < length) {
        size_t end = pos;
        while (end + 1 < length && !(packet[end] == '\r' && packet[end + 1] == '\n')) ++end;
        if (end == pos) break;
        if (end - pos > name_length && packet[pos + name_length] == ':') {
            bool match = true;
            for (size_t i = 0; i < name_length; ++i)
                if (std::tolower(packet[pos + i]) != std::tolower(static_cast<unsigned char>(name[i]))) match = false;
            if (match) {
                size_t value = pos + name_length + 1;
                while (value < end && (packet[value] == ' ' || packet[value] == '\t')) ++value;
                while (end > value && (packet[end - 1] == ' ' || packet[end - 1] == '\t')) --end;
                return std::string(reinterpret_cast<const char *>(packet + value), end - value);
            }
        }
        if (end + 1 >= length) break;
        pos = end + 2;
    }
    return {};
}
std::vector<std::string> scrubbed_sip_lines(const uint8_t *packet, size_t length) {
    // Secrets never reach the log: digest headers are replaced before printing,
    // together with any folded continuation lines of theirs (RFC 3261 7.3.1),
    // and so are the SRTP keys an SDP body carries in a=crypto (RFC 4568),
    // which SDES and OSRTP put in the signalling. The file this goes to
    // outlives the call, and a key in it would unlock a captured recording.
    static const char *secrets[] = {"authorization", "proxy-authorization", "www-authenticate", "proxy-authenticate"};
    std::string text(reinterpret_cast<const char *>(packet), length);
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
        auto colon = line.find(':');
        if (colon != std::string::npos) {
            std::string name = lower(line.substr(0, colon));
            for (auto secret : secrets)
                if (name == secret) {
                    line = line.substr(0, colon) + ": ***";
                    hiding = true;
                    break;
                }
        }
        if (line.compare(0, 9, "a=crypto:") == 0) {
            auto key = line.find(" inline:");
            if (key != std::string::npos) line = line.substr(0, key) + " inline:***";
        }
        if (!line.empty()) lines.push_back(line);
    }
    return lines;
}
void log_sip_message(bool tx, const uint8_t *packet, size_t length) {
    for (auto &line : scrubbed_sip_lines(packet, length)) info("ksip sip %s %s\n", tx ? ">" : "<", line.c_str());
}
DialogInfo read_dialog_info(const std::string &body) {
    DialogInfo info;
    auto attribute = [&](const std::string &attrs, const char *name) {
        auto at = attrs.find(std::string(name) + "=\"");
        if (at == std::string::npos) return std::string();
        at += strlen(name) + 2;
        auto end = attrs.find('"', at);
        return end == std::string::npos ? std::string() : attrs.substr(at, end - at);
    };
    std::string dialog_id;
    int in_dialog = 0;
    for (size_t pos = body.find('<'); pos != std::string::npos; pos = body.find('<', pos)) {
        auto end = body.find('>', pos);
        if (end == std::string::npos) break;
        std::string tag = body.substr(pos + 1, end - pos - 1);
        pos = end + 1;
        if (tag.empty() || tag[0] == '?' || tag[0] == '!') continue;
        bool closing = tag[0] == '/';
        if (closing) tag.erase(0, 1);
        tag = trim(tag);
        bool self_closing = !tag.empty() && tag.back() == '/';
        if (self_closing) {
            tag.pop_back();
            tag = trim(tag);
        }
        auto space = tag.find_first_of(" \t\r\n");
        std::string name = lower(tag.substr(0, space)), attrs = space == std::string::npos ? std::string() : tag.substr(space);
        auto colon = name.find(':');
        if (colon != std::string::npos) name.erase(0, colon + 1);
        if (name == "dialog-info") {
            if (!closing) {
                info.readable = true;
                info.partial = lower(attribute(attrs, "state")) == "partial";
            }
        } else if (name == "dialog") {
            if (closing) {
                if (in_dialog) --in_dialog;
            } else if (!self_closing) {
                ++in_dialog;
                dialog_id = attribute(attrs, "id");
            }
        } else if (name == "state" && !closing && !self_closing && in_dialog) {
            auto text_end = body.find('<', pos);
            std::string text = lower(trim(body.substr(pos, text_end == std::string::npos ? std::string::npos : text_end - pos)));
            info.dialogs.emplace_back(dialog_id, text);
        }
    }
    return info;
}
std::string codec_list(const std::string &names) {
    static const std::pair<const char *, const char *> known[] = {
        {"opus", "opus/48000/1"}, {"G722", "G722/16000/1"}, {"PCMU", "PCMU/8000/1"}, {"PCMA", "PCMA/8000/1"}};
    std::string codecs;
    for (size_t start = 0; start <= names.size();) {
        auto end = names.find(',', start);
        std::string name = names.substr(start, end == std::string::npos ? std::string::npos : end - start);
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
    for (char ch : value) user += ch == '#' ? "%23" : std::string(1, ch);
    return user;
}
bool parse_action(const char *prm, ActionRequest &request) {
    odict *od = nullptr;
    if (!prm || json_decode_odict(&od, 16, prm, strlen(prm), 4)) return false;
    request.op = odict_string(od, "op") ? odict_string(od, "op") : "";
    request.id = odict_string(od, "id") ? odict_string(od, "id") : "";
    request.value = odict_string(od, "value") ? odict_string(od, "value") : "";
    mem_deref(od);
    return true;
}
bool parse_watch_list(const char *prm, std::array<std::string, 30> &values) {
    if (!prm || !*prm) return false;
    std::string text = prm;
    size_t start = 0, count = 0;
    for (;;) {
        if (count == values.size()) return false;
        auto end = text.find(',', start);
        values[count] = text.substr(start, end == std::string::npos ? end : end - start);
        if (!values[count].empty() &&
            !(sip_uri(values[count]) ? address_ok(values[count]) : (values[count].size() <= 30 && token(values[count].c_str(), "*#+"))))
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
    std::string text = prm;
    auto comma = text.find(',');
    if (comma == std::string::npos) return false;
    microphone = text.substr(0, comma);
    speaker = text.substr(comma + 1);
    for (const auto &device : {microphone, speaker}) {
        if (device.empty() || device.size() >= limit) return false;
        for (unsigned char ch : device) if (ch < 0x20 || ch == 0x7f || ch == ',') return false;
    }
    return true;
}
void add_audio_stats(odict *od, const ksip_audio_stats &stats) {
    odict *aec = nullptr;
    if (odict_alloc(&aec, 24)) return;
    if (stats.flags & KSIP_AUDIO_STATS_ECHO_RETURN_LOSS) odict_entry_add(aec, "echo_return_loss", ODICT_DOUBLE, stats.echo_return_loss);
    if (stats.flags & KSIP_AUDIO_STATS_ECHO_RETURN_LOSS_ENHANCEMENT) odict_entry_add(aec, "echo_return_loss_enhancement", ODICT_DOUBLE, stats.echo_return_loss_enhancement);
    if (stats.flags & KSIP_AUDIO_STATS_DELAY) odict_entry_add(aec, "delay_ms", ODICT_INT, static_cast<int64_t>(stats.delay_ms));
    if (stats.flags & KSIP_AUDIO_STATS_DIVERGENT_FILTER_FRACTION) odict_entry_add(aec, "divergent_filter_fraction", ODICT_DOUBLE, stats.divergent_filter_fraction);
    if (stats.flags & KSIP_AUDIO_STATS_DELAY_MEDIAN) odict_entry_add(aec, "delay_median_ms", ODICT_INT, static_cast<int64_t>(stats.delay_median_ms));
    if (stats.flags & KSIP_AUDIO_STATS_DELAY_STANDARD_DEVIATION) odict_entry_add(aec, "delay_standard_deviation_ms", ODICT_INT, static_cast<int64_t>(stats.delay_standard_deviation_ms));
    if (stats.flags & KSIP_AUDIO_STATS_RESIDUAL_ECHO_LIKELIHOOD) odict_entry_add(aec, "residual_echo_likelihood", ODICT_DOUBLE, stats.residual_echo_likelihood);
    if (stats.flags & KSIP_AUDIO_STATS_RESIDUAL_ECHO_LIKELIHOOD_RECENT_MAX) odict_entry_add(aec, "residual_echo_likelihood_recent_max", ODICT_DOUBLE, stats.residual_echo_likelihood_recent_max);
    if (stats.flags & KSIP_AUDIO_STATS_RENDER_LEVEL) odict_entry_add(aec, "render_rms_dbfs", ODICT_DOUBLE, stats.render_rms_dbfs);
    if (stats.flags & KSIP_AUDIO_STATS_CAPTURE_DEVICE_LEVEL) odict_entry_add(aec, "capture_device_rms_dbfs", ODICT_DOUBLE, stats.capture_device_rms_dbfs);
    if (stats.flags & KSIP_AUDIO_STATS_CAPTURE_MONO_LEVEL) odict_entry_add(aec, "capture_mono_rms_dbfs", ODICT_DOUBLE, stats.capture_mono_rms_dbfs);
    if (stats.flags & KSIP_AUDIO_STATS_CAPTURE_INPUT_LEVEL) odict_entry_add(aec, "capture_input_rms_dbfs", ODICT_DOUBLE, stats.capture_input_rms_dbfs);
    if (stats.flags & KSIP_AUDIO_STATS_CAPTURE_OUTPUT_LEVEL) odict_entry_add(aec, "capture_output_rms_dbfs", ODICT_DOUBLE, stats.capture_output_rms_dbfs);
    if (stats.flags & KSIP_AUDIO_STATS_AGC) {
        odict_entry_add(aec, "agc_speech_level_dbfs", ODICT_DOUBLE, stats.agc_speech_level_dbfs);
        odict_entry_add(aec, "agc_noise_level_dbfs", ODICT_DOUBLE, stats.agc_noise_level_dbfs);
        odict_entry_add(aec, "agc_headroom_db", ODICT_DOUBLE, stats.agc_headroom_db);
        odict_entry_add(aec, "agc_gain_db", ODICT_DOUBLE, stats.agc_gain_db);
    }
    odict_entry_add(aec, "stream_delay_ms", ODICT_INT, static_cast<int64_t>(stats.stream_delay_ms));
    odict_entry_add(aec, "stream_delay_from_device", ODICT_BOOL, stats.stream_delay_from_device != 0);
    odict_entry_add(aec, "render_frames", ODICT_INT, static_cast<int64_t>(stats.render_frames));
    odict_entry_add(aec, "capture_frames", ODICT_INT, static_cast<int64_t>(stats.capture_frames));
    odict_entry_add(aec, "render_errors", ODICT_INT, static_cast<int64_t>(stats.render_errors));
    odict_entry_add(aec, "capture_errors", ODICT_INT, static_cast<int64_t>(stats.capture_errors));
    odict_entry_add(aec, "capture_device_rate", ODICT_INT, static_cast<int64_t>(stats.capture_device_rate));
    odict_entry_add(aec, "capture_device_channels", ODICT_INT, static_cast<int64_t>(stats.capture_device_channels));
    odict_entry_add(od, "audio_processing_stats", ODICT_OBJECT, aec);
    mem_deref(aec);
}
} // namespace ksip_io
