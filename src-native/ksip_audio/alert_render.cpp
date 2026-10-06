// The alert sounds' WASAPI stream; see alert_render.h. Apart from the other
// files of the module, since the Windows audio headers it needs define
// macros (near, far) the rest of the module uses as names. What is done
// here is what baresip's wasapi module did for a sound, split between the
// opening thread and the feeding one.
#include <windows.h>
#include <mmdeviceapi.h>
#include <audioclient.h>
#include <mmreg.h>
#include <cerrno>
#include <climits>
#include <cstring>
#include <vector>
#include <string>
#include <cstdint>
#include <atomic>
#include <re.h>
#include <baresip.h>
#include "alert_render.h"

namespace alert_player {
namespace {
constexpr REFERENCE_TIME kRefPerMs = 10000;
constexpr uint32_t kMsPerS = 1000;
// How long the feeder waits when the buffer has no room for the next block.
constexpr DWORD kFullWaitMs = 5;
std::wstring wide(const std::string &utf8) {
    const int n = MultiByteToWideChar(CP_UTF8, 0, utf8.data(), static_cast<int>(utf8.size()), nullptr, 0);
    std::wstring out(n > 0 ? static_cast<size_t>(n) : 0, L'\0');
    if (n > 0) MultiByteToWideChar(CP_UTF8, 0, utf8.data(), static_cast<int>(utf8.size()), out.data(), n);
    return out;
}
std::string narrow(const wchar_t *text) {
    const int n = WideCharToMultiByte(CP_UTF8, 0, text, -1, nullptr, 0, nullptr, nullptr);
    std::string out(n > 1 ? static_cast<size_t>(n - 1) : 0, '\0');
    if (n > 1) WideCharToMultiByte(CP_UTF8, 0, text, -1, out.data(), n, nullptr, nullptr);
    return out;
}
// Windows' result as the core counts it: no such endpoint, the sound's
// format refused, or anything else.
int errno_of(HRESULT hr) {
    if (hr == HRESULT_FROM_WIN32(ERROR_NOT_FOUND) || hr == AUDCLNT_E_DEVICE_INVALIDATED) return ENODEV;
    if (hr == E_INVALIDARG || hr == AUDCLNT_E_UNSUPPORTED_FORMAT) return EINVAL;
    return EIO;
}
template <class T>
void release(T *&object) {
    if (object) object->Release();
    object = nullptr;
}
} // namespace

struct WasapiRender::Wasapi {
    Alert *alert = nullptr;
    IAudioClient *client = nullptr;
    IAudioRenderClient *service = nullptr;
    WAVEFORMATEX *format = nullptr;
    UINT32 buffer_frames = 0, frames = 0;
    // One block of the sound's samples, as the handler fills it.
    std::vector<uint8_t> samples;
    size_t sampc = 0;
    bool started = false;
    bool com = false;
};

int WasapiRender::open(const std::string &endpoint, void *player) {
    close();
    auto *w = new Wasapi;
    w->alert = static_cast<Alert *>(player);
    // The thread has COM already (the bridge's, in the multithreaded
    // model); asked for here too, so that this does not depend on it.
    w->com = SUCCEEDED(CoInitializeEx(nullptr, COINIT_MULTITHREADED));
    const bool chosen = endpoint != "default";
    const char *step = "the device list";
    IMMDeviceEnumerator *devices = nullptr;
    IMMDevice *device = nullptr;
    HRESULT hr = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, __uuidof(IMMDeviceEnumerator), reinterpret_cast<void **>(&devices));
    if (SUCCEEDED(hr)) {
        step = chosen ? "the speaker" : "the default communications speaker";
        hr = chosen ? devices->GetDevice(wide(endpoint).c_str(), &device) : devices->GetDefaultAudioEndpoint(eRender, eCommunications, &device);
    }
    if (SUCCEEDED(hr)) {
        step = "the endpoint's id";
        LPWSTR wid = nullptr;
        hr = device->GetId(&wid);
        if (SUCCEEDED(hr)) {
            id = narrow(wid);
            CoTaskMemFree(wid);
        }
    }
    if (SUCCEEDED(hr)) {
        step = "IMMDevice::Activate";
        hr = device->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, reinterpret_cast<void **>(&w->client));
    }
    if (SUCCEEDED(hr)) {
        step = "IAudioClient::GetMixFormat";
        hr = w->client->GetMixFormat(&w->format);
    }
    const auplay_prm &prm = w->alert->prm;
    if (SUCCEEDED(hr)) {
        // The sound's own format; Windows converts (AUTOCONVERTPCM).
        WAVEFORMATEX *f = w->format;
        f->wFormatTag = WAVE_FORMAT_PCM;
        f->nChannels = prm.ch;
        f->nSamplesPerSec = prm.srate;
        f->wBitsPerSample = static_cast<WORD>(aufmt_sample_size(static_cast<enum aufmt>(prm.fmt)) * CHAR_BIT);
        f->nBlockAlign = static_cast<WORD>((f->wBitsPerSample / CHAR_BIT) * f->nChannels);
        f->nAvgBytesPerSec = f->nSamplesPerSec * f->nBlockAlign;
        f->cbSize = 0;
        step = "IAudioClient::Initialize";
        hr = w->client->Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                                   static_cast<REFERENCE_TIME>(prm.ptime) * kRefPerMs * 2, 0, f, nullptr);
    }
    if (SUCCEEDED(hr)) {
        step = "IAudioClient::GetService";
        hr = w->client->GetService(__uuidof(IAudioRenderClient), reinterpret_cast<void **>(&w->service));
    }
    if (SUCCEEDED(hr)) {
        step = "IAudioClient::GetBufferSize";
        hr = w->client->GetBufferSize(&w->buffer_frames);
    }
    if (SUCCEEDED(hr)) {
        step = "IAudioClient::Start";
        hr = w->client->Start();
    }
    release(device);
    release(devices);
    wasapi = w;
    if (FAILED(hr)) {
        warning("ksip_alert: %s failed (0x%08lx), the alert sound's stream did not open on %s\n", step, static_cast<unsigned long>(hr), endpoint.c_str());
        close();
        return errno_of(hr);
    }
    w->started = true;
    w->frames = prm.srate * prm.ptime / kMsPerS;
    w->sampc = static_cast<size_t>(w->frames) * prm.ch;
    w->samples.assign(aufmt_sample_size(static_cast<enum aufmt>(prm.fmt)) * w->sampc, 0);
    stopped.store(false, std::memory_order_relaxed);
    run.store(true, std::memory_order_relaxed);
    thread = std::thread([this] { feed(); });
    return 0;
}

void WasapiRender::feed() {
    Wasapi *w = wasapi;
    const bool com = SUCCEEDED(CoInitializeEx(nullptr, COINIT_MULTITHREADED));
    const auplay_prm &prm = w->alert->prm;
    auframe af{};
    auframe_init(&af, static_cast<enum aufmt>(prm.fmt), w->samples.data(), w->sampc, prm.srate, prm.ch);
    const char *step = nullptr;
    HRESULT hr = S_OK;
    while (run.load(std::memory_order_relaxed)) {
        UINT32 padding = 0;
        hr = w->client->GetCurrentPadding(&padding);
        if (FAILED(hr)) {
            step = "IAudioClient::GetCurrentPadding";
            break;
        }
        if (w->buffer_frames - padding < w->frames) {
            Sleep(kFullWaitMs);
            continue;
        }
        w->alert->handler(&af, w->alert->arg);
        BYTE *out = nullptr;
        hr = w->service->GetBuffer(w->frames, &out);
        if (FAILED(hr)) {
            step = "IAudioRenderClient::GetBuffer";
            break;
        }
        std::memcpy(out, w->samples.data(), static_cast<size_t>(w->format->nBlockAlign) * w->frames);
        hr = w->service->ReleaseBuffer(w->frames, 0);
        if (FAILED(hr)) {
            step = "IAudioRenderClient::ReleaseBuffer";
            break;
        }
    }
    if (step) {
        warning("ksip_alert: %s failed (0x%08lx), the alert sound's stream on %s ended\n", step, static_cast<unsigned long>(hr), id.c_str());
        stopped.store(true, std::memory_order_relaxed);
    }
    if (com) CoUninitialize();
}

void WasapiRender::close() {
    run.store(false, std::memory_order_relaxed);
    if (thread.joinable()) thread.join();
    if (Wasapi *w = wasapi) {
        wasapi = nullptr;
        if (w->started) w->client->Stop();
        release(w->service);
        release(w->client);
        if (w->format) CoTaskMemFree(w->format);
        if (w->com) CoUninitialize();
        delete w;
    }
    stopped.store(false, std::memory_order_relaxed);
    id.clear();
}
} // namespace alert_player
