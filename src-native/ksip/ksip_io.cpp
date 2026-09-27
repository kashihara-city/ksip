// The module's reading and writing that touches libre or baresip; see ksip_io.h.
#define WIN32_LEAN_AND_MEAN
#include "ksip_io.h"
#include <cstring>

namespace ksip_io {
std::string display_name(const pl &name) {
    if (!pl_isset(&name)) return "";
    std::string s(name.p, name.l);
    while (!s.empty() && (s.back() == ' ' || s.back() == '\t')) s.pop_back();
    if (s.size() >= 2 && s.front() == '"' && s.back() == '"') s = s.substr(1, s.size() - 2);
    return s;
}
void log_sip_message(bool tx, const uint8_t *packet, size_t length) {
    for (auto &line : ksip_text::scrubbed_sip_lines(packet, length)) info("ksip sip %s %s\n", tx ? ">" : "<", line.c_str());
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
