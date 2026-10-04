// The alert sounds' device bridge, as their session core (session_core.h)
// sees one: one rendering stream at a time, opened on the endpoint the
// device asked for stands for, as the call's bridge opens its streams
// (device_selection.cc): the device while it is listed, the default
// communications speaker in its place while it is not. The stream is a
// `Render`: alert_render.h's WASAPI stream, or a test's record. A render
// opens on the calling thread, so that the result and the endpoint it got
// (the endpoint's own id, also for the default asked for) are known when
// open() returns, as the call's bridge has them; and it says when it ended
// on its own (the device went, the audio service stopped), which the core
// takes as the endpoint gone and opens the stream again.
//
// What a Render is:
//   int open(const std::string &endpoint, void *player)  0, or an errno;
//                                 on 0, `id` is the endpoint opened, by its id
//   void close()                  the stream stopped and let go
//   bool ended() const            the stream stopped on its own since open()
//   std::string id
//
// Threads: the core's (baresip's main thread). No baresip here, so that
// the tests build it.
#pragma once
#include "session_core.h"
#include <cerrno>
#include <functional>
#include <string>
#include <utility>

namespace alert_player {
template <class Render>
struct RenderAdm final : playback_session::Adm {
    struct Hooks {
        // Whether a speaker is there, by the test the calls go by; "default"
        // while there is any.
        std::function<bool(const char *id)> usable;
        // A device asked for that is not there, said once per open.
        std::function<void(const std::string &asked)> not_there;
    };
    Render render;
    Hooks hooks;
    // The player the render feeds, and the device it was asked for.
    void *current = nullptr;
    std::string request;
    explicit RenderAdm(Hooks hooks) : hooks(std::move(hooks)) {}
    // The running render serves the device asked for (the bridge's Serves,
    // in the small): it is on an endpoint that is there, and that is the one
    // asked for, or the default was asked for, or the one asked for is not there.
    bool serves(const std::string &asked) const {
        if (!current || render.ended() || !hooks.usable(render.id.c_str())) return false;
        return asked == "default" || render.id == asked || !hooks.usable(asked.c_str());
    }
    int start_playout(const char *device, void *player) override {
        const std::string asked = device && device[0] ? device : "default";
        if (player == current && request == asked && serves(asked)) return 0;
        stop_playout();
        const bool chosen = asked != "default";
        const std::string endpoint = chosen && hooks.usable(asked.c_str()) ? asked : "default";
        if (chosen && endpoint != asked) hooks.not_there(asked);
        const int err = render.open(endpoint, player);
        if (err) return err;
        current = player;
        request = asked;
        return 0;
    }
    // The sound's handler is the render's own: letting go of it is stopping it.
    void detach_playout() override { stop_playout(); }
    bool playout_running() override { return current != nullptr; }
    void stop_playout() override {
        if (current) render.close();
        current = nullptr;
        request.clear();
    }
    int start_recording(const char *, void *) override { return ENOSYS; }
    void detach_recording() override {}
    bool recording_running() override { return false; }
    void stop_recording() override {}
    // The endpoint the stream is on; none once it ended on its own.
    std::string opened(bool playout) override { return playout && current && !render.ended() ? render.id : std::string(); }
    bool listed(const char *device, bool) override { return hooks.usable(device); }
};
} // namespace alert_player
