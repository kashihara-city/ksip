// The audio device module: registers the KSIP source and player with
// baresip, brings the WebRTC bridge up on the settings, and hands each
// request for a player or a source to the session, which decides who has
// the stream; and the player of the alert sounds (alert_player.h).
#include "audio_session.h"
#include "audio_state.h"
#include "alert_player.h"
#include "microphone_mute.h"
#include <cerrno>
#include <cstring>

namespace {
struct auplay *g_player;
struct ausrc *g_source;
ksip_audio *g_audio;
// Whether the module is up, and with any of the processing on; see audio_state.h.
bool g_ready = false, g_processing = false;
int StateCommand(re_printf *pf, void *) {
    odict *od = nullptr;
    int err = odict_alloc(&od, 4);
    if (!err) err = ksip_audio_add_state(od);
    if (!err) err = json_encode_odict(pf, od);
    mem_deref(od);
    return err;
}
// The devices the configuration names now (ksip_audio_devices has just set
// them) taken by the streams that are up: a change made during a call
// reaches that call. A side this module does not have is left alone.
int SwitchCommand(re_printf *pf, void *) {
    const struct config *cfg = conf_config();
    if (!cfg || !g_ready) return ENODEV;
    const auto chosen = [](const char *module, const char *device) -> const char * {
        if (str_cmp(module, "ksip_audio")) return nullptr;
        return device[0] ? device : "default";
    };
    playback_session::switch_devices(chosen(cfg->audio.src_mod, cfg->audio.src_dev), chosen(cfg->audio.play_mod, cfg->audio.play_dev));
    return re_hprintf(pf, "Audio streams on the configured devices\n");
}
const cmd commands[] = {
    {"ksip_audio_state", 0, 0, "The audio module's state as JSON (audio_state.h)", StateCommand},
    {"ksip_audio_switch", 0, 0, "Move the streams that are up onto the configured devices", SwitchCommand},
};

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
    bool aec = true, high_pass = true, agc = false, detail = false, raw_microphone = true, raw_speaker = true;
    char level[24] = "high";
    conf_get_u32(conf_cur(), "webrtc_aec_delay_ms", &delay);
    conf_get_bool(conf_cur(), "ksip_aec_enabled", &aec);
    conf_get_bool(conf_cur(), "ksip_high_pass", &high_pass);
    conf_get_str(conf_cur(), "ksip_noise_suppression", level, sizeof level);
    conf_get_bool(conf_cur(), "ksip_agc", &agc);
    conf_get_bool(conf_cur(), "ksip_detail_log", &detail);
    // RAW mode for the streams, so that no effects of the device change their
    // audio; both by default, each its own switch for a setting to come.
    conf_get_bool(conf_cur(), "ksip_raw_microphone", &raw_microphone);
    conf_get_bool(conf_cur(), "ksip_raw_speaker", &raw_speaker);
    ksip_audio_set_raw(raw_microphone, raw_speaker);
    const ksip_audio_processing processing{aec, high_pass, NoiseLevel(level), agc};
    const bool enabled = aec || high_pass || processing.noise_suppression >= 0 || agc;
    // Set before creating, so that the device warnings of a failing start show up.
    ksip_audio_set_log_level(detail);
    const int result = ksip_audio_create(std::min(delay, 500u), &processing, &g_audio);
    if (result) return ENODEV;
    playback_session::open(g_audio);
    int error = ausrc_register(&g_source, baresip_ausrcl(), "ksip_audio", AllocateSource);
    error |= auplay_register(&g_player, baresip_auplayl(), "ksip_audio", AllocatePlayout);
    error |= alert_player::start();
    error |= cmd_register(baresip_commands(), commands, RE_ARRAY_SIZE(commands));
    if (error) {
        cmd_unregister(baresip_commands(), commands);
        alert_player::stop();
        g_source = static_cast<struct ausrc *>(mem_deref(g_source));
        g_player = static_cast<struct auplay *>(mem_deref(g_player));
        playback_session::close();
        ksip_audio_destroy(g_audio);
        g_audio = nullptr;
        return error;
    }
    g_ready = true;
    g_processing = enabled;
    microphone_mute::start(g_audio);
    // For the log only: the app reads the module's state (audio_state.h).
    if (enabled)
        info("ksip_audio: Google WebRTC ADM + APM initialized (processing enabled:"
             " aec %s, high-pass %s, noise suppression %s, agc %s)\n",
             aec ? "on" : "off", high_pass ? "on" : "off", processing.noise_suppression >= 0 ? level : "off", agc ? "on" : "off");
    else info("ksip_audio: Google WebRTC ADM + APM initialized (processing disabled)\n");
    return 0;
}
int module_close() {
    microphone_mute::stop();
    g_ready = false;
    g_processing = false;
    cmd_unregister(baresip_commands(), commands);
    alert_player::stop();
    playback_session::close();
    g_source = static_cast<struct ausrc *>(mem_deref(g_source));
    g_player = static_cast<struct auplay *>(mem_deref(g_player));
    ksip_audio_destroy(g_audio);
    g_audio = nullptr;
    return 0;
}
} // namespace

extern "C" int ksip_audio_add_state(struct odict *od) {
    if (!od) return EINVAL;
    odict *audio = nullptr;
    int err = odict_alloc(&audio, 8);
    if (err) return err;
    odict_entry_add(audio, "ready", ODICT_BOOL, g_ready);
    odict_entry_add(audio, "processing", ODICT_BOOL, g_ready && g_processing);
    // Not up, the session has had nothing to do: no input, no output, no
    // failures, which is what it says.
    playback_session::add_state(audio);
    // Whether the last capture and playout streams took RAW mode; not there before the first.
    if (const int raw = g_audio ? ksip_audio_capture_raw() : 0) odict_entry_add(audio, "capture_raw", ODICT_BOOL, raw == 1);
    if (const int raw = g_audio ? ksip_audio_playout_raw() : 0) odict_entry_add(audio, "playout_raw", ODICT_BOOL, raw == 1);
    err = odict_entry_add(od, "audio", ODICT_OBJECT, audio);
    mem_deref(audio);
    return err;
}
extern "C" int ksip_audio_get_current_stats(ksip_audio_stats *stats) {
    if (!stats || !g_audio) return EINVAL;
    return ksip_audio_get_stats(g_audio, stats);
}
extern "C" const struct mod_export DECL_EXPORTS(ksip_audio) = {"ksip_audio", "sound", module_init, module_close};
