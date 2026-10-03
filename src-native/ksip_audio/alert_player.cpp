// The alert sounds' player; see alert_player.h. Apart from the other files of
// the module, as microphone_mute.cpp is, for the Windows audio headers.
#include <windows.h>
#include <mmdeviceapi.h>
#include <re.h>
#include <baresip.h>
#include <string>
#include "alert_player.h"

namespace alert_player {
namespace {
struct auplay *g_player = nullptr;
// What baresip holds for a sound: the wasapi player doing the work. baresip
// only passes the pointer around and dereferences it.
struct Alert {
    struct auplay_st *inner;
};
void destroy(void *arg) { mem_deref(static_cast<Alert *>(arg)->inner); }

// Whether Windows has the render endpoint with this id and can use it now
// (DEVICE_STATE_ACTIVE: not unplugged, not disabled), the test the bridge
// makes against the endpoints WebRTC lists, which are the active ones.
bool usable(const char *id) {
    // On baresip's main thread COM is up already (microphone_mute.cpp); a
    // thread without it gets it for the look.
    const HRESULT com = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    IMMDeviceEnumerator *devices = nullptr;
    IMMDevice *device = nullptr;
    DWORD state = 0;
    HRESULT hr = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&devices));
    if (SUCCEEDED(hr)) {
        // Endpoint ids are plain ASCII ({0.0.0.00000000}.{GUID}).
        std::wstring wide(id, id + strlen(id));
        hr = devices->GetDevice(wide.c_str(), &device);
    }
    if (SUCCEEDED(hr)) hr = device->GetState(&state);
    if (device) device->Release();
    if (devices) devices->Release();
    if (SUCCEEDED(com)) CoUninitialize();
    return SUCCEEDED(hr) && (state & DEVICE_STATE_ACTIVE);
}

int allocate(struct auplay_st **out, const struct auplay *, struct auplay_prm *prm, const char *device,
             auplay_write_h *handler, void *arg) {
    if (!out || !prm) return EINVAL;
    const bool chosen = device && *device && str_casecmp(device, "default") != 0;
    const char *endpoint = chosen && usable(device) ? device : "default";
    if (chosen && endpoint != device)
        warning("ksip_alert: speaker %s is not there, the alert sound plays on the default communications speaker\n", device);
    auto alert = static_cast<Alert *>(mem_zalloc(sizeof(Alert), destroy));
    if (!alert) return ENOMEM;
    const int err = auplay_alloc(&alert->inner, baresip_auplayl(), "wasapi", prm, endpoint, handler, arg);
    if (err) {
        mem_deref(alert);
        return err;
    }
    *out = reinterpret_cast<struct auplay_st *>(alert);
    return 0;
}
} // namespace

int start() { return auplay_register(&g_player, baresip_auplayl(), "ksip_alert", allocate); }
void stop() { g_player = static_cast<struct auplay *>(mem_deref(g_player)); }
} // namespace alert_player
