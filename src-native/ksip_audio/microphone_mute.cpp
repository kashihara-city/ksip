// The microphone's mute carried into the calls; see microphone_mute.h. Apart
// from the other files of the module, since the Windows audio headers it
// needs define macros (near, far) the rest of the module uses as names.
#include <windows.h>
#include <mmdeviceapi.h>
#include <endpointvolume.h>
#include <re.h>
#include <baresip.h>
#include <string>
#include <cstdint>
#include <cstring>
#include "ksip_audio_bridge.h"
#include "microphone_mute.h"

namespace microphone_mute {
namespace {
// How often the mute is read: a call muted on the device sends silence
// within this, the call that starts while muted included.
constexpr uint64_t kEveryMs = 250;
ksip_audio *g_bridge = nullptr;
tmr g_timer;
bool g_com = false;
// What the last reading said, so that the log has the changes only.
bool g_known = false, g_muted = false;

// The mute of the endpoint the bridge records from: its id, or, with none
// (the default asked for), the default communications capture endpoint, as
// the bridge opens it (device_selection.cc). Looked up afresh each time, so
// that a default Windows moves elsewhere is followed. False when it cannot
// be read (no such endpoint now).
bool read_mute(const char *id, bool &muted) {
    IMMDeviceEnumerator *devices = nullptr;
    IMMDevice *device = nullptr;
    IAudioEndpointVolume *volume = nullptr;
    HRESULT hr = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&devices));
    if (SUCCEEDED(hr)) {
        if (id && *id) {
            // Endpoint ids are plain ASCII ({0.0.1.00000000}.{GUID}).
            const std::wstring wide(id, id + strlen(id));
            hr = devices->GetDevice(wide.c_str(), &device);
        } else {
            hr = devices->GetDefaultAudioEndpoint(eCapture, eCommunications, &device);
        }
    }
    if (SUCCEEDED(hr)) hr = device->Activate(__uuidof(IAudioEndpointVolume), CLSCTX_ALL, nullptr, reinterpret_cast<void **>(&volume));
    BOOL value = FALSE;
    if (SUCCEEDED(hr)) hr = volume->GetMute(&value);
    if (volume) volume->Release();
    if (device) device->Release();
    if (devices) devices->Release();
    if (FAILED(hr)) return false;
    muted = value != FALSE;
    return true;
}
// Every call's audio follows the device: muted while it is, back when it is
// not. Only the app's own calls are muted this way (nothing else in KSIP
// mutes baresip's audio), so following the device undoes nothing.
void apply(bool muted) {
    for (const le *u = list_head(uag_list()); u; u = u->next)
        for (const le *l = list_head(ua_calls(static_cast<ua *>(u->data))); l; l = l->next) {
            audio *a = call_audio(static_cast<call *>(l->data));
            if (a && audio_ismuted(a) != muted) audio_mute(a, muted);
        }
}
void tick(void *) {
    tmr_start(&g_timer, kEveryMs, tick, nullptr);
    if (!g_bridge) return;
    // The endpoint the capture stream actually opened, the default WebRTC
    // moved to after the one in use went away included; before any stream,
    // the one selected (ksip_audio_get_device_info).
    ksip_audio_device_info device{};
    const char *id = !ksip_audio_get_device_info(g_bridge, &device) ? device.recording_id : nullptr;
    bool muted = false;
    if (!id || !read_mute(id, muted)) {
        // A mute that cannot be read any more (the endpoint gone) is not kept
        // on the calls: what was muted for it comes back.
        if (g_known && g_muted) {
            info("ksip_audio: the microphone's mute cannot be read; calls send what it picks up\n");
            apply(false);
        }
        g_known = false;
        return;
    }
    if (g_known ? muted != g_muted : muted)
        info("ksip_audio: the microphone is %s on the device; calls send %s\n", muted ? "muted" : "unmuted",
             muted ? "silence" : "what it picks up");
    g_known = true;
    g_muted = muted;
    apply(muted);
}
} // namespace

void start(ksip_audio *bridge) {
    g_bridge = bridge;
    g_known = false;
    // The timer runs on this thread, which reads the mute through COM. A
    // thread that has COM already (in either model) can use it as it is.
    const HRESULT hr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    g_com = SUCCEEDED(hr);
    tmr_init(&g_timer);
    tmr_start(&g_timer, kEveryMs, tick, nullptr);
}
void stop() {
    tmr_cancel(&g_timer);
    g_bridge = nullptr;
    if (g_com) CoUninitialize();
    g_com = false;
}
} // namespace microphone_mute
