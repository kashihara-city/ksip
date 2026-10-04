// One WASAPI rendering stream for an alert sound (alert_adm.h's Render):
// opened on the calling thread, from the device to IAudioClient::Start, so
// that whether it opened, and on which endpoint, is known when open()
// returns; fed from a thread of its own, which takes each block of samples
// from the sound's handler, as baresip's wasapi module did; and ended by
// that thread when the stream fails under it (the device went, the audio
// service stopped), which ended() tells.
//
// Threads: open() and close() on baresip's main thread; the feed thread
// calls the sound's handler and nothing else of the module's. close()
// joins it, so that the handler is never called after the sound is freed.
#pragma once
#include <re.h>
#include <rem.h>
#include <baresip.h>
#include <atomic>
#include <string>
#include <thread>

namespace alert_player {
// What baresip holds for a sound: the device asked for, whether the sound
// is up, and its write handler, given to each render in turn.
struct Alert {
    auplay_write_h *handler;
    void *arg;
    auplay_prm prm;
    bool started;
    char device_name[160];
    const char *device() const { return device_name; }
    void set_device(const char *device) { str_ncpy(device_name, device && device[0] ? device : "default", sizeof(device_name)); }
};

struct WasapiRender {
    // The endpoint opened, by its own id (the default asked for included).
    std::string id;
    WasapiRender() = default;
    WasapiRender(const WasapiRender &) = delete;
    WasapiRender &operator=(const WasapiRender &) = delete;
    ~WasapiRender() { close(); }
    // The stream on the endpoint ("default": the default communications
    // speaker) for the sound, started; 0, or an errno (ENODEV: no such
    // endpoint, EINVAL: the sound's format refused, EIO: anything else),
    // the step that failed and Windows' result in the log.
    int open(const std::string &endpoint, void *player);
    void close();
    bool ended() const { return stopped.load(std::memory_order_relaxed); }

private:
    struct Wasapi;
    Wasapi *wasapi = nullptr;
    std::atomic<bool> run{false}, stopped{false};
    std::thread thread;
    void feed();
};
} // namespace alert_player
