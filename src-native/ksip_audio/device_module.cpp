// The audio device module: registers the KSIP source and player with
// baresip, brings the WebRTC bridge up on the settings, and hands each
// request for a player or a source to the session, which decides who has
// the stream.
#include "audio_session.h"
#include <cerrno>
#include <cstring>

namespace {
struct auplay *g_player;
struct ausrc *g_source;
ksip_audio *g_audio;

bool Valid(const struct auplay_prm *p) { return p->srate == playback_session::kRate && p->ch == playback_session::kChannels && p->fmt == AUFMT_S16LE; }
bool Valid(const struct ausrc_prm *p) { return p->srate == playback_session::kRate && p->ch == playback_session::kChannels && p->fmt == AUFMT_S16LE; }
int AllocatePlayout(struct auplay_st **out, const struct auplay *, struct auplay_prm *p, const char *device, auplay_write_h *handler, void *arg) {
    if (!out || !p || !handler || !playback_session::bridge()) return EINVAL;
    if (!Valid(p)) return ENOTSUP;
    return playback_session::allocate_playout(out, device, handler, arg);
}
int AllocateSource(struct ausrc_st **out, const struct ausrc *, struct ausrc_prm *p, const char *device, ausrc_read_h *handler, ausrc_error_h *,
                   void *arg) {
    if (!out || !p || !handler || !playback_session::bridge()) return EINVAL;
    if (!Valid(p)) return ENOTSUP;
    return playback_session::allocate_source(out, device, handler, arg);
}
// The setting's word for the noise suppression, as the bridge counts it.
int NoiseLevel(const char *level) {
    static const char *const names[] = {"low", "moderate", "high", "very_high"};
    for (int i = 0; i < 4; ++i)
        if (!std::strcmp(level, names[i])) return i;
    return std::strcmp(level, "off") ? 2 : -1;
}
int module_init() {
    uint32_t delay = 20;
    bool aec = true, high_pass = true, agc = false, detail = false;
    char level[24] = "high";
    conf_get_u32(conf_cur(), "webrtc_aec_delay_ms", &delay);
    conf_get_bool(conf_cur(), "ksip_aec_enabled", &aec);
    conf_get_bool(conf_cur(), "ksip_high_pass", &high_pass);
    conf_get_str(conf_cur(), "ksip_noise_suppression", level, sizeof level);
    conf_get_bool(conf_cur(), "ksip_agc", &agc);
    conf_get_bool(conf_cur(), "ksip_detail_log", &detail);
    const ksip_audio_processing processing{aec, high_pass, NoiseLevel(level), agc};
    const bool enabled = aec || high_pass || processing.noise_suppression >= 0 || agc;
    // Set before creating, so that the device warnings of a failing start show up.
    ksip_audio_set_log_level(detail);
    const int result = ksip_audio_create(std::min(delay, 500u), &processing, &g_audio);
    if (result) return ENODEV;
    playback_session::open(g_audio);
    int error = ausrc_register(&g_source, baresip_ausrcl(), "ksip_audio", AllocateSource);
    error |= auplay_register(&g_player, baresip_auplayl(), "ksip_audio", AllocatePlayout);
    if (error) {
        g_source = static_cast<struct ausrc *>(mem_deref(g_source));
        g_player = static_cast<struct auplay *>(mem_deref(g_player));
        playback_session::close();
        ksip_audio_destroy(g_audio);
        g_audio = nullptr;
        return error;
    }
    // The app reads "processing enabled" from this line.
    if (enabled)
        info("ksip_audio: Google WebRTC ADM + APM initialized (processing enabled:"
             " aec %s, high-pass %s, noise suppression %s, agc %s)\n",
             aec ? "on" : "off", high_pass ? "on" : "off", processing.noise_suppression >= 0 ? level : "off", agc ? "on" : "off");
    else info("ksip_audio: Google WebRTC ADM + APM initialized (processing disabled)\n");
    return 0;
}
int module_close() {
    playback_session::close();
    g_source = static_cast<struct ausrc *>(mem_deref(g_source));
    g_player = static_cast<struct auplay *>(mem_deref(g_player));
    ksip_audio_destroy(g_audio);
    g_audio = nullptr;
    return 0;
}
} // namespace

extern "C" int ksip_audio_get_current_stats(ksip_audio_stats *stats) {
    if (!stats || !g_audio) return EINVAL;
    return ksip_audio_get_stats(g_audio, stats);
}
extern "C" const struct mod_export DECL_EXPORTS(ksip_audio) = {"ksip_audio", "sound", module_init, module_close};
